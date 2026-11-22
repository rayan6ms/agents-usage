use crate::config::{self, AppConfig};
use crate::domain::{RateWindow, UsageSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

pub const ID: &str = "antigravity";
pub const IMPORT_COMMAND: &str =
    "agy -p /usage --output-format json | agents-usage --import-antigravity-usage";
const MAX_REPORT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SavedQuota {
    windows: Vec<QuotaWindow>,
    email: Option<String>,
    plan: Option<String>,
    saved_at: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct QuotaWindow {
    id: String,
    remaining: f32,
    reset_at: Option<i64>,
}

fn window_kind(id: &str) -> Option<(&'static str, u64, u8)> {
    match id {
        "gemini-5h" => Some(("5-hour", 300, 0)),
        "3p-5h" => Some(("Claude/GPT 5-hour", 300, 1)),
        "3p-weekly" => Some(("Claude/GPT weekly", 10_080, 2)),
        "gemini-argon-wtu-weekly" => Some(("Argon weekly", 10_080, 3)),
        "gemini-carbon-wtu-weekly" => Some(("Carbon weekly", 10_080, 4)),
        "gemini-weekly" => Some(("Weekly", 10_080, 5)),
        _ => None,
    }
}

fn text(value: Option<&Value>, limit: usize) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty() && value.chars().count() <= limit && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

impl SavedQuota {
    fn parse(bytes: &[u8], now: i64) -> Result<(Self, bool), &'static str> {
        let report: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
                .map_err(|_| "Expected JSON from agy -p /usage --output-format json.")?;
        let status_line = report.get("product").and_then(Value::as_str) == Some(ID)
            && report.get("quota").is_some_and(Value::is_object);
        let mut windows = Vec::new();
        let mut add_window = |id: &str, bucket: &Value| {
            let Some((_, duration, order)) = window_kind(id) else {
                return;
            };
            if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
                return;
            }
            let Some(remaining) = bucket
                .get("remaining_fraction")
                .and_then(Value::as_f64)
                .filter(|value| (0.0..=1.0).contains(value))
            else {
                return;
            };
            if windows
                .iter()
                .any(|(_, window): &(u8, QuotaWindow)| window.id == id)
            {
                return;
            }
            let reset_at = bucket
                .get("reset_time")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|time| time.timestamp())
                .or_else(|| {
                    let seconds = bucket.get("reset_in_seconds")?.as_i64()?;
                    (seconds >= 0).then_some(())?;
                    now.checked_add(seconds)
                })
                .filter(|time| {
                    // Unused buckets may carry a rolling deadline one full window away.
                    !(remaining == 1.0
                        && bucket.get("description").is_none()
                        && time.abs_diff(now.saturating_add(duration as i64 * 60)) <= 60)
                });
            windows.push((
                order,
                QuotaWindow {
                    id: id.into(),
                    remaining: remaining as f32,
                    reset_at,
                },
            ));
        };
        let data = if status_line {
            if let Some(buckets) = report.get("quota").and_then(Value::as_object) {
                for (id, bucket) in buckets {
                    add_window(id, bucket);
                }
            }
            &report
        } else {
            if report.get("status").and_then(Value::as_str) != Some("SUCCESS")
                || !matches!(
                    report.pointer("/command/name").and_then(Value::as_str),
                    Some("usage" | "quota")
                )
            {
                return Err(
                    "Antigravity did not return a successful quota report. Run agy and check your sign-in.",
                );
            }
            for group in report
                .pointer("/command/data/groups")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for bucket in group
                    .get("buckets")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(id) = bucket.get("id").and_then(Value::as_str) {
                        add_window(id, bucket);
                    }
                }
            }
            report
                .pointer("/command/data")
                .ok_or("Antigravity did not include quota details.")?
        };
        if windows.is_empty() {
            return Err("Antigravity did not include any supported quota windows.");
        }
        windows.sort_by_key(|(order, _)| *order);
        Ok((
            Self {
                windows: windows.into_iter().map(|(_, window)| window).collect(),
                email: text(data.get("email"), 254),
                plan: text(data.get("plan_tier"), 40),
                saved_at: now,
            },
            status_line,
        ))
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        UsageSnapshot {
            email: self.email.clone(),
            plan_type: self.plan.clone(),
            bucket_name: Some("Antigravity".into()),
            windows: self
                .windows
                .iter()
                .filter_map(|window| {
                    let (label, duration_mins, _) = window_kind(&window.id)?;
                    Some(RateWindow {
                        label: Some(label.into()),
                        used_percent: (1.0 - window.remaining) * 100.0,
                        duration_mins: Some(duration_mins),
                        resets_at: window.reset_at,
                    })
                })
                .collect(),
            reset_available_count: 0,
            reset_credits: Vec::new(),
        }
    }

    pub fn saved_label(&self) -> String {
        chrono::DateTime::from_timestamp(self.saved_at, 0)
            .map(|time| {
                format!(
                    "Imported {}",
                    time.with_timezone(&chrono::Local).format("%b %e at %H:%M")
                )
            })
            .unwrap_or_else(|| "Imported quota report".into())
    }
}

pub fn manual_home() -> PathBuf {
    config::config_path().with_file_name("manual-antigravity")
}

fn report_path() -> PathBuf {
    config::config_path().with_file_name("antigravity-usage.json")
}

pub fn read_report() -> Option<SavedQuota> {
    let mut bytes = Vec::new();
    fs::File::open(report_path())
        .ok()?
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub fn clear_report() -> io::Result<()> {
    match fs::remove_file(report_path()) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub fn import_report(input: impl Read) -> Result<(), String> {
    let mut bytes = Vec::new();
    input
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the Antigravity report.".to_string())?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return Err("The Antigravity report is too large.".into());
    }
    let (mut report, status_line) =
        SavedQuota::parse(&bytes, chrono::Utc::now().timestamp()).map_err(str::to_owned)?;
    // The official status hook may repeat a payload frequently while typing.
    // Avoid rewriting an unchanged snapshot or making it look freshly fetched.
    if status_line && let Some(previous) = read_report() {
        let received_at = report.saved_at;
        report.saved_at = previous.saved_at;
        if report == previous {
            return Ok(());
        }
        report.saved_at = received_at;
    }
    let data =
        serde_json::to_vec(&report).map_err(|_| "Could not encode the Antigravity report.")?;
    config::atomic_write(&report_path(), &data)
        .map_err(|error| format!("Could not save the Antigravity report: {error}"))
}

pub fn set_quota(config: &mut AppConfig, report: Option<SavedQuota>) {
    if let Some(value) = &report {
        let home = manual_home();
        let has_preference = config
            .accounts
            .iter()
            .any(|account| account.provider_id == ID && account.home == home);
        if !has_preference
            && let Some(previous) = config
                .accounts
                .iter_mut()
                .find(|account| account.provider_id == ID)
        {
            if previous.display_name.as_deref()
                == previous
                    .identity_email
                    .as_deref()
                    .and_then(|email| email.split('@').next())
            {
                previous.display_name = None;
            }
            previous.home = home.clone();
            // Older releases could leave this preference disabled after the live source was paused.
            previous.enabled = true;
            previous.expanded = true;
        }
        let pref = config::preference_for_provider_mut(config, ID, &home);
        pref.identity_email.clone_from(&value.email);
        if !has_preference {
            pref.expanded = true;
        }
    }
    config.antigravity_report = report;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_usage_report_keeps_shared_windows_and_provider_reset() {
        let payload = json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[{"name":"Gemini Models","buckets":[
            {"id":"gemini-weekly","remaining_fraction":0.75,"reset_time":"2026-11-23T03:00:00Z"},
            {"id":"gemini-5h","remaining_fraction":0.0},
            {"id":"3p-weekly","remaining_fraction":1.0,"disabled":true}
        ]}]}}});
        let (report, status_line) =
            SavedQuota::parse(payload.to_string().as_bytes(), 1_799_000_000).unwrap();
        assert!(!status_line);
        let usage = report.snapshot();
        assert_eq!(usage.email, None);
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].used_percent, 100.0);
        assert_eq!(usage.windows[1].used_percent, 25.0);
        assert_eq!(usage.windows[1].resets_at, Some(1795402800));
    }

    #[test]
    fn status_line_keeps_only_quota_identity_and_plan_and_rejects_empty_or_failed_reports() {
        let input = br#"{"product":"antigravity","email":"user@example.com","plan_tier":"Google AI Pro","transcript_path":"private","quota":{"gemini-weekly":{"remaining_fraction":0.5,"reset_in_seconds":3601},"gemini-5h":{"remaining_fraction":1,"reset_in_seconds":18000}}}"#;
        let (report, status_line) = SavedQuota::parse(input, 0).unwrap();
        assert!(status_line);
        assert_eq!(
            report.snapshot().plan_type.as_deref(),
            Some("Google AI Pro")
        );
        let windows = report.snapshot().windows;
        assert_eq!(windows[0].label.as_deref(), Some("5-hour"));
        assert_eq!(windows[0].resets_at, None);
        assert_eq!(windows[1].label.as_deref(), Some("Weekly"));
        assert_eq!(windows[1].resets_at, Some(3601));
        assert!(!serde_json::to_string(&report).unwrap().contains("private"));
        for input in [
            "{}",
            r#"{"status":"ERROR"}"#,
            r#"{"product":"antigravity","quota":{"gemini-weekly":{"remaining_fraction":1.1}}}"#,
        ] {
            assert!(SavedQuota::parse(input.as_bytes(), 0).is_err());
        }
    }
}
