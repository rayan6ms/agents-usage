//! Reconcile local sign-ins before selecting accounts for network refreshes.
//! Preferences belong to a provider/home pair, never to an email address.
use crate::config::{AccountPreference, AppConfig};
use crate::domain::{CachedUsage, UsageSnapshot};
use crate::{account_id, antigravity, codex, normalized_account_email, providers};
use std::collections::HashMap;
use std::path::PathBuf;

pub fn identity_snapshot(email: String) -> UsageSnapshot {
    UsageSnapshot {
        email: Some(email), plan_type: None, bucket_name: None, windows: Vec::new(),
        reset_available_count: 0, reset_credits: Vec::new(),
    }
}

pub fn reconcile(
    config: &mut AppConfig,
    cache: Vec<CachedUsage>,
    homes: Vec<PathBuf>,
    antigravity_homes: Vec<PathBuf>,
) -> Vec<CachedUsage> {
    // Read every home, including disabled ones, exactly once per pass. auth_time
    // measures a login; file mtime/iat alone could favor a routine token refresh.
    let mut identities = HashMap::new();
    let mut paths = homes;
    paths.extend(config.accounts.iter().filter(|p| p.provider_id == providers::OPENAI).map(|p| p.home.clone()));
    for home in paths {
        let id = account_id(providers::OPENAI, &home);
        if identities.contains_key(&id) { continue; }
        let identity = codex::local_auth_identity(&home).ok().flatten();
        if let Some((email, _)) = &identity {
            if !config.accounts.iter().any(|p| account_id(&p.provider_id, &p.home) == id) {
                config.accounts.push(AccountPreference {
                    home: home.clone(), identity_email: Some(email.clone()),
                    ..AccountPreference::default()
                });
            }
        }
        identities.insert(id, identity);
    }

    let mut cached = cache.into_iter().map(|c| (account_id(&c.provider_id, &c.home), c.snapshot)).collect::<HashMap<_, _>>();

    let mut ag_paths = antigravity_homes;
    ag_paths.extend(
        config
            .accounts
            .iter()
            .filter(|p| p.provider_id == antigravity::ID && !antigravity::is_manual_home(&p.home))
            .map(|p| p.home.clone()),
    );
    let mut ag_emails = std::collections::HashSet::new();
    for home in ag_paths {
        if antigravity::is_manual_home(&home) { continue; }
        let id = account_id(antigravity::ID, &home);
        if identities.contains_key(&id) { continue; }
        let identity = antigravity::local_identity(&home);
        if let Some((email, _)) = &identity {
            ag_emails.insert(email.clone());
            if !config.accounts.iter().any(|p| account_id(&p.provider_id, &p.home) == id) {
                if let Some(manual) = config.accounts.iter_mut().find(|p| {
                    p.provider_id == antigravity::ID
                        && antigravity::is_manual_home(&p.home)
                        && normalized_account_email(p.identity_email.as_deref()).as_ref() == Some(email)
                }) {
                    let old_id = account_id(antigravity::ID, &manual.home);
                    manual.home = home.clone();
                    manual.identity_email = Some(email.clone());
                    if let Some(snapshot) = cached.remove(&old_id).or_else(|| {
                        config.antigravity_report.as_ref().and_then(|report| {
                            let snap = report.snapshot();
                            (normalized_account_email(snap.email.as_deref()).as_ref() == Some(email))
                                .then_some(snap)
                        })
                    }) {
                        cached.insert(id.clone(), snapshot);
                    }
                } else {
                    config.accounts.push(AccountPreference {
                        provider_id: antigravity::ID.into(),
                        home: home.clone(),
                        identity_email: Some(email.clone()),
                        ..AccountPreference::default()
                    });
                }
            }
        }
        identities.insert(id, identity);
    }

    if !ag_emails.is_empty() {
        config.accounts.retain(|pref| {
            !(pref.provider_id == antigravity::ID
                && antigravity::is_manual_home(&pref.home)
                && normalized_account_email(pref.identity_email.as_deref())
                    .is_none_or(|email| ag_emails.contains(&email)))
        });
        if config
            .antigravity_report
            .as_ref()
            .is_some_and(|report| antigravity::report_covered_by_t3(config, report))
        {
            config.antigravity_report = None;
        }
    }

    let mut groups = HashMap::<(String, String), Vec<(usize, i64)>>::new();
    let mut result = Vec::new();
    for (index, pref) in config.accounts.iter_mut().enumerate() {
        let id = account_id(&pref.provider_id, &pref.home);
        let local = identities.get(&id).and_then(Option::as_ref);
        let mut snapshot = cached.get(&id).cloned();
        if pref.provider_id == antigravity::ID && antigravity::is_manual_home(&pref.home) {
            snapshot = config
                .antigravity_report
                .as_ref()
                .map(antigravity::SavedQuota::snapshot);
        } else if let Some((email, login_time)) = local {
            let previous = pref.identity_email.as_deref();
            if normalized_account_email(previous).as_ref() != Some(email) {
                // Update an old auto-generated email label, but preserve custom names.
                if pref.display_name.as_deref() == previous.and_then(|e| e.split('@').next()) {
                    pref.display_name = None;
                }
            }
            pref.identity_email = Some(email.clone());
            if normalized_account_email(snapshot.as_ref().and_then(|s| s.email.as_deref())).as_ref() != Some(email) {
                // Never show the previous identity's quota or redeemable credits.
                snapshot = Some(identity_snapshot(email.clone()));
            }
            groups.entry((pref.provider_id.clone(), email.clone())).or_default().push((index, *login_time));
        } else if pref.identity_email.is_none() {
            pref.identity_email = normalized_account_email(snapshot.as_ref().and_then(|s| s.email.as_deref()));
        }
        if let Some(snapshot) = snapshot {
            result.push(CachedUsage { provider_id: pref.provider_id.clone(), home: pref.home.clone(), snapshot });
        }
    }

    for ((provider_id, _), group) in groups {
        // On timestamp ties retain the current selection; sorting/reordering
        // settings must not change the active home.
        let winner = group.iter().max_by_key(|(i, time)| (
            *time, config.accounts[*i].duplicate_of.is_none(), config.accounts[*i].enabled,
            std::cmp::Reverse(account_id(&provider_id, &config.accounts[*i].home)),
        )).unwrap().0;
        let was_auto_disabled = config.accounts[winner].duplicate_of.is_some();
        let transfer_enabled = group.iter().any(|(i, _)| *i != winner && config.accounts[*i].enabled);
        if was_auto_disabled || transfer_enabled { config.accounts[winner].enabled = true; }
        config.accounts[winner].duplicate_of = None;
        let home = config.accounts[winner].home.clone();
        for &(index, _) in &group {
            if index == winner { continue; }
            let pref = &mut config.accounts[index];
            if pref.enabled || pref.duplicate_of.is_some() {
                pref.enabled = false;
                pref.duplicate_of = Some(home.clone());
            }
        }
    }
    result
}
