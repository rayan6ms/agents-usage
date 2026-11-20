use crate::domain::{RateWindow, UsageSnapshot};
use crate::providers::{self, ProviderCandidate, ProviderError, ProviderReading};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

pub const ID: &str = "antigravity";
const LABEL: &str = "Antigravity";
const PROFILE_DIR: &str = "antigravity-acp";
const TOKEN_FILE: &str = "antigravity-oauth-token";
const USER_AGENT: &str = "antigravity/hub/2.24.0";
const HOSTS: [&str; 2] = [
    "daily-cloudcode-pa.googleapis.com",
    "cloudcode-pa.googleapis.com",
];

#[derive(Deserialize)]
struct Credential {
    token: Token,
    auth_method: String,
    id_token_email: String,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    expiry: String,
}

impl Token {
    fn expired(&self) -> bool {
        DateTime::parse_from_rfc3339(&self.expiry).map_or(true, |expiry| expiry <= Utc::now())
    }
}

fn credential(profile: &Path) -> Option<Credential> {
    let profile = profile.join(PROFILE_DIR);
    let settings: Value =
        serde_json::from_slice(&read_profile_file(&profile.join("settings.json"))?).ok()?;
    if settings.pointer("/auth/type")?.as_str()? != "oauth-personal" {
        return None;
    }
    let credential: Credential =
        serde_json::from_slice(&read_profile_file(&profile.join(TOKEN_FILE))?).ok()?;
    (credential.auth_method == "consumer" && !credential.id_token_email.trim().is_empty())
        .then_some(credential)
}

fn read_profile_file(path: &Path) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 65_536).then_some(bytes)
}

pub fn local_email(profile: &Path) -> Option<String> {
    credential(profile).map(|value| value.id_token_email)
}

pub fn candidates(home: &Path) -> Vec<ProviderCandidate> {
    let t3 = std::env::var_os("T3CODE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".t3"));
    let Ok(profiles) = fs::read_dir(t3.join("userdata/providers/antigravity")) else {
        return Vec::new();
    };
    let mut homes: Vec<_> = profiles
        .flatten()
        .map(|entry| entry.path())
        .filter(|profile| local_email(profile).is_some())
        .collect();
    homes.sort();
    homes
        .into_iter()
        .map(|home| ProviderCandidate {
            provider_id: ID.into(),
            home,
        })
        .collect()
}

fn sign_in_error() -> ProviderError {
    ProviderError::Message(
        "Open Antigravity in T3 Code to renew its Google sign-in, then refresh".into(),
    )
}

fn request_error(source: reqwest::Error) -> ProviderError {
    if matches!(
        source.status(),
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    ) {
        return sign_in_error();
    }
    ProviderError::Request {
        provider: LABEL,
        source,
    }
}

async fn google_post(client: &Client, token: &str, method: &str) -> Result<Value, ProviderError> {
    let mut error = ProviderError::Message("Antigravity could not be reached".into());
    for host in HOSTS {
        let response = client
            .post(format!("https://{host}/v1internal:{method}"))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status);
        match response {
            Ok(response) => return response.json().await.map_err(request_error),
            Err(source) => error = request_error(source),
        }
    }
    Err(error)
}

fn plan_name(value: &Value) -> Option<String> {
    let paid_tier = value
        .pointer("/paidTier/name")
        .and_then(Value::as_str)
        .map(str::trim);
    if let Some(name) = paid_tier.filter(|name| !name.is_empty()) {
        return Some(name.into());
    }
    match value.pointer("/currentTier/id").and_then(Value::as_str) {
        Some("free-tier") => Some("Free".into()),
        _ => None,
    }
}

pub async fn read_account(profile: &Path) -> Result<ProviderReading, ProviderError> {
    let mut sign_in = credential(profile).ok_or_else(sign_in_error)?;
    if sign_in.token.expired() {
        refresh_sign_in(profile).await?;
        sign_in = credential(profile).ok_or_else(sign_in_error)?;
    }
    if sign_in.token.expired() || sign_in.token.access_token.is_empty() {
        return Err(sign_in_error());
    }

    let client = providers::http_client(LABEL)?;
    let token = &sign_in.token.access_token;
    let (plan, quota) = tokio::try_join!(
        google_post(&client, token, "loadCodeAssist"),
        google_post(&client, token, "retrieveUserQuotaSummary"),
    )?;
    let windows = quota_windows(&quota, Utc::now().timestamp());
    if windows.is_empty() {
        return Err(ProviderError::Message(
            "Antigravity returned no usable quota".into(),
        ));
    }
    Ok(ProviderReading {
        snapshot: UsageSnapshot {
            email: Some(sign_in.id_token_email),
            plan_type: plan_name(&plan),
            bucket_name: Some(LABEL.into()),
            windows,
            reset_available_count: 0,
            reset_credits: Vec::new(),
        },
        notice: None,
    })
}

fn quota_windows(quota: &Value, now: i64) -> Vec<RateWindow> {
    let groups = quota
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let grouped_buckets = groups.flat_map(|group| {
        let name = group.get("displayName").and_then(Value::as_str);
        group
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(move |bucket| (name, bucket))
    });
    let flat_buckets = quota
        .get("buckets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|bucket| (None, bucket));
    let mut seen = HashSet::new();
    let mut windows = Vec::new();
    for (family, bucket) in grouped_buckets.chain(flat_buckets) {
        if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(id) = bucket.get("bucketId").and_then(Value::as_str) else {
            continue;
        };
        let Some(remaining) = bucket.get("remainingFraction").and_then(Value::as_f64) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let (duration_mins, period) = match bucket.get("window").and_then(Value::as_str) {
            Some("5h") => (Some(300), "5-hour"),
            Some("weekly") => (Some(10_080), "Weekly"),
            _ => (
                None,
                bucket
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("Quota"),
            ),
        };
        let (family, order) = match id {
            "gemini-5h" => (Some("Gemini"), 0),
            "gemini-weekly" => (Some("Gemini"), 2),
            "3p-5h" | "3p-weekly" => (Some("Claude & GPT"), 1),
            _ => (family, 1),
        };
        let label = family.map_or_else(|| period.into(), |name| format!("{name} · {period}"));
        let resets_at = providers::timestamp(bucket.get("resetTime")).filter(|reset| {
            // Google can return a rolling placeholder while a window is unused.
            !(remaining == 1.0
                && bucket.get("description").is_none()
                && duration_mins
                    .is_some_and(|duration| (now + duration as i64 * 60).abs_diff(*reset) <= 60))
        });
        windows.push((
            order,
            RateWindow {
                label: Some(label),
                used_percent: ((1.0 - remaining.clamp(0.0, 1.0)) * 100.0) as f32,
                duration_mins,
                resets_at,
            },
        ));
    }
    // The compact UI takes the first short and last equally long window.
    windows.sort_by_key(|(order, _)| *order);
    windows.into_iter().map(|(_, window)| window).collect()
}

fn managed_executable(profile: &Path) -> Option<PathBuf> {
    // Profile layout: <t3 root>/userdata/providers/antigravity/<profile>.
    let root = profile.ancestors().nth(4)?;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        os => os,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    let runtime = root
        .join("tools")
        .join(PROFILE_DIR)
        .join(format!("{os}-{arch}"));
    let active: Value =
        serde_json::from_slice(&read_profile_file(&runtime.join("active.json"))?).ok()?;
    let release = active.get("releaseId")?.as_str()?;
    if release.len() != 64 || !release.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let executable = if cfg!(windows) {
        "agy.exe"
    } else {
        "antigravity"
    };
    let executable = runtime.join("versions").join(release).join(executable);
    executable.is_file().then_some(executable)
}

async fn refresh_sign_in(profile: &Path) -> Result<(), ProviderError> {
    let executable = managed_executable(profile).ok_or_else(sign_in_error)?;
    // Let Google's runtime renew its own credentials without starting a conversation.
    let mut child = Command::new(executable)
        .args(["acp", "--gemini-dir"])
        .arg(profile)
        .args(["--app-data-dir", PROFILE_DIR, "--disable-telemetry"])
        .current_dir(profile)
        .env("GEMINI_HOME", profile)
        .env("AGY_ACP_FORCE_FILE_STORAGE", "1")
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "1")
        .env("SSH_CONNECTION", "127.0.0.1 0 127.0.0.1 0")
        .env("BROWSER", "false")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("AGY_ADC_AUTH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| sign_in_error())?;
    let mut stdin = child.stdin.take().ok_or_else(sign_in_error)?;
    let stdout = child.stdout.take().ok_or_else(sign_in_error)?;
    let mut lines = BufReader::new(stdout.take(256 * 1024)).lines();

    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = [
            ("initialize", json!({"protocolVersion": 1, "clientInfo": {"name": "agents-usage", "version": env!("CARGO_PKG_VERSION")}})),
            ("authenticate", json!({"methodId": "oauth-personal"})),
        ];
        for (index, (method, params)) in requests.into_iter().enumerate() {
            let id = index + 1;
            let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            stdin.write_all(format!("{request}\n").as_bytes()).await.map_err(|_| sign_in_error())?;
            loop {
                let line = lines.next_line().await.map_err(|_| sign_in_error())?.ok_or_else(sign_in_error)?;
                let response: Value = serde_json::from_str(&line).map_err(|_| sign_in_error())?;
                if response.get("id").and_then(Value::as_u64) == Some(id as u64) {
                    if response.get("result").is_none() || response.get("error").is_some() {
                        return Err(sign_in_error());
                    }
                    break;
                }
            }
        }
        drop(stdin);
        child.wait().await.map_err(|_| sign_in_error())?;
        Ok(())
    }).await.map_err(|_| sign_in_error())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paid_plan_wins_over_google_base_tier() {
        let response = json!({
            "currentTier": {"id": "free-tier", "name": "Antigravity"},
            "paidTier": {"id": "g1-pro-tier", "name": "Google AI Pro"},
        });
        assert_eq!(plan_name(&response).as_deref(), Some("Google AI Pro"));
        assert_eq!(
            plan_name(&json!({"currentTier": {"id": "free-tier"}})).as_deref(),
            Some("Free")
        );
        assert_eq!(plan_name(&json!({})), None);
    }

    #[test]
    fn quota_keeps_the_gemini_windows_and_suppresses_rolling_idle_resets() {
        const NOW: i64 = 1_800_000_000;
        let response = json!({"groups": [
            {"displayName": "Gemini Models", "buckets": [
                {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.75, "resetTime": NOW + 604_800},
                {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 1.0, "resetTime": NOW + 18_000}
            ]},
            {"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1.0, "resetTime": NOW + 604_800, "disabled": true},
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 0.5, "resetTime": NOW + 10_000}
            ]}
        ], "buckets": [
            {"bucketId": "gemini-5h", "remainingFraction": 0.2},
            {"bucketId": "unknown"}
        ]});
        let windows = quota_windows(&response, NOW);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label.as_deref(), Some("Gemini · 5-hour"));
        assert_eq!(windows[0].resets_at, None);
        assert_eq!(windows[1].label.as_deref(), Some("Claude & GPT · 5-hour"));
        assert_eq!(windows[1].resets_at, Some(NOW + 10_000));
        assert_eq!(windows[2].label.as_deref(), Some("Gemini · Weekly"));
        assert_eq!(windows[2].used_percent, 25.0);
        assert_eq!(windows[2].resets_at, Some(NOW + 604_800));
    }

    #[test]
    fn discovery_ignores_a_saved_personal_login_after_switching_to_api_key() {
        let profile =
            std::env::temp_dir().join(format!("agents-usage-antigravity-{}", uuid::Uuid::new_v4()));
        let directory = profile.join(PROFILE_DIR);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(TOKEN_FILE),
            json!({
                "auth_method": "consumer",
                "id_token_email": "example@example.com",
                "token": {"access_token": "fixture", "expiry": "2026-11-20T17:50:37-03:00"},
            })
            .to_string(),
        )
        .unwrap();
        let settings = directory.join("settings.json");
        fs::write(&settings, r#"{"auth":{"type":"oauth-personal"}}"#).unwrap();
        let signed_in = local_email(&profile);
        fs::write(&settings, r#"{"auth":{"type":"gemini-api-key"}}"#).unwrap();
        let switched = local_email(&profile);
        fs::remove_dir_all(&profile).unwrap();
        assert_eq!(signed_in.as_deref(), Some("example@example.com"));
        assert_eq!(switched, None);
    }
}
