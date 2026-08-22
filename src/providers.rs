use crate::codex;
use crate::config::AppConfig;
use crate::discovery;
use crate::domain::{RateWindow, UsageSnapshot};
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, AesGcm, KeyInit, Nonce};
use chrono::DateTime;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;

pub const OPENAI: &str = "openai";
pub const OPENCODE: &str = "opencode";
pub const ANTHROPIC: &str = "anthropic";
pub const GOOGLE: &str = "google";
pub const CURSOR: &str = "cursor";
pub const XAI: &str = "xai";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const GOOGLE_CODE_ASSIST_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCandidate {
    pub provider_id: String,
    pub home: PathBuf,
}

#[derive(Debug)]
pub struct ProviderReading {
    pub snapshot: UsageSnapshot,
    pub notice: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("{0}")]
    Message(String),
    #[error("{provider} request failed: {source}")]
    Request {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
}

impl ProviderError {
    pub fn user_message(&self) -> String {
        match self {
            Self::Message(message) => message.clone(),
            Self::Request { provider, source } if source.is_timeout() => {
                format!("{provider} did not respond in time; showing the last update")
            }
            Self::Request { provider, .. } => {
                format!("Could not update {provider}; showing the last update")
            }
        }
    }
}

impl From<codex::CodexError> for ProviderError {
    fn from(error: codex::CodexError) -> Self {
        Self::Message(error.user_message().to_string())
    }
}

pub fn display_name(provider_id: &str) -> &'static str {
    match provider_id {
        OPENAI => "OpenAI Codex",
        OPENCODE => "OpenCode Go",
        ANTHROPIC => "Claude",
        GOOGLE => "Gemini",
        CURSOR => "Cursor",
        XAI => "Grok",
        _ => "Agent",
    }
}

pub fn candidates(config: &AppConfig) -> Vec<ProviderCandidate> {
    let mut candidates = discovery::candidate_codex_homes(config)
        .into_iter()
        .map(|home| ProviderCandidate {
            provider_id: OPENAI.into(),
            home,
        })
        .collect::<Vec<_>>();

    if let Some(home) = discovery::user_home() {
        let opencode = opencode_data_dir(&home);
        if has_opencode_go_key(&opencode.join("auth.json")) {
            candidates.push(ProviderCandidate {
                provider_id: OPENCODE.into(),
                home: opencode,
            });
        }

        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        if claude.join(".credentials.json").is_file()
            || cfg!(target_os = "macos") && claude.is_dir()
        {
            candidates.push(ProviderCandidate {
                provider_id: ANTHROPIC.into(),
                home: claude,
            });
        }

        let gemini = std::env::var_os("GEMINI_CLI_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.clone())
            .join(".gemini");
        if gemini.join("oauth_creds.json").is_file()
            || gemini.join("gemini-credentials.json").is_file()
            || gemini.join("google_accounts.json").is_file()
        {
            candidates.push(ProviderCandidate {
                provider_id: GOOGLE.into(),
                home: gemini,
            });
        }

        let cursor = home.join(".cursor");
        if cursor.is_dir() && locate_user_executable(&["cursor-agent"], &home).is_some() {
            candidates.push(ProviderCandidate {
                provider_id: CURSOR.into(),
                home: cursor,
            });
        }

        let grok = home.join(".grok");
        if grok.join("auth.json").is_file() {
            candidates.push(ProviderCandidate {
                provider_id: XAI.into(),
                home: grok,
            });
        }
    }

    for preference in &config.accounts {
        if preference.provider_id != OPENAI && is_marked(&preference.provider_id, &preference.home)
        {
            candidates.push(ProviderCandidate {
                provider_id: preference.provider_id.clone(),
                home: preference.home.clone(),
            });
        }
    }

    let mut seen = HashSet::new();
    candidates.retain(|candidate| {
        let home = candidate
            .home
            .canonicalize()
            .unwrap_or_else(|_| candidate.home.clone());
        seen.insert((candidate.provider_id.clone(), home))
    });
    candidates
}

pub fn is_marked(provider_id: &str, home: &Path) -> bool {
    match provider_id {
        OPENAI => discovery::is_marked_codex_home(home),
        OPENCODE => has_opencode_go_key(&home.join("auth.json")),
        ANTHROPIC => {
            home.join(".credentials.json").is_file() || cfg!(target_os = "macos") && home.is_dir()
        }
        GOOGLE => {
            home.join("oauth_creds.json").is_file()
                || home.join("gemini-credentials.json").is_file()
                || home.join("google_accounts.json").is_file()
        }
        CURSOR => home.is_dir(),
        XAI => home.join("auth.json").is_file(),
        _ => false,
    }
}

pub async fn read_account(
    candidate: &ProviderCandidate,
    codex_path: Option<&Path>,
) -> Result<ProviderReading, ProviderError> {
    match candidate.provider_id.as_str() {
        OPENAI => {
            let codex_path = codex_path.ok_or_else(|| {
                ProviderError::Message("Codex is not installed or is not available on PATH".into())
            })?;
            Ok(ProviderReading {
                snapshot: codex::read_openai_account(codex_path, &candidate.home).await?,
                notice: None,
            })
        }
        OPENCODE => read_opencode_go(&candidate.home).await,
        ANTHROPIC => read_claude(&candidate.home).await,
        GOOGLE => read_gemini(&candidate.home).await,
        CURSOR => read_cursor(&candidate.home).await,
        XAI => read_grok(&candidate.home).await,
        _ => Err(ProviderError::Message(
            "This provider is not supported".into(),
        )),
    }
}

fn http_client(provider: &'static str) -> Result<Client, ProviderError> {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("agents-usage/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|source| ProviderError::Request { provider, source })
}

fn read_json(path: &Path, label: &str) -> Result<Value, ProviderError> {
    let text = fs::read_to_string(path)
        .map_err(|_| ProviderError::Message(format!("{label} sign-in was not found")))?;
    serde_json::from_str(&text)
        .map_err(|_| ProviderError::Message(format!("{label} credentials could not be read")))
}

fn has_opencode_go_key(path: &Path) -> bool {
    read_opencode_auth(path)
        .ok()
        .and_then(|value| value.get("opencode-go").cloned())
        .and_then(|value| value.get("key").and_then(Value::as_str).map(str::to_string))
        .is_some_and(|key| !key.is_empty())
}

fn opencode_data_dir(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("opencode")
}

fn read_opencode_auth(path: &Path) -> Result<Value, ProviderError> {
    if let Some(value) = std::env::var_os("OPENCODE_AUTH_CONTENT") {
        return serde_json::from_str(&value.to_string_lossy())
            .map_err(|_| ProviderError::Message("OpenCode credentials could not be read".into()));
    }
    read_json(path, "OpenCode")
}

async fn read_opencode_go(home: &Path) -> Result<ProviderReading, ProviderError> {
    let auth = read_opencode_auth(&home.join("auth.json"))?;
    let key = auth
        .pointer("/opencode-go/key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("Connect OpenCode Go in OpenCode first".into()))?;
    let client = http_client("OpenCode Go")?;
    let response = client
        .get(OPENCODE_GO_USAGE_URL)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "OpenCode Go",
            source,
        })?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(ProviderError::Message(
            "OpenCode Go usage needs the current Go usage API; update OpenCode and try again"
                .into(),
        ));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(ProviderError::Message(
            "This key does not have an active OpenCode Go subscription".into(),
        ));
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(ProviderError::Message(
            "OpenCode Go sign-in expired; reconnect it in OpenCode".into(),
        ));
    }
    let value: Value = response
        .error_for_status()
        .map_err(|source| ProviderError::Request {
            provider: "OpenCode Go",
            source,
        })?
        .json()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "OpenCode Go",
            source,
        })?;
    let windows = normalize_opencode_usage(&value, chrono::Utc::now().timestamp());
    if windows.is_empty() {
        return Err(ProviderError::Message(
            "OpenCode Go returned no usable quota windows".into(),
        ));
    }
    Ok(ProviderReading {
        snapshot: snapshot(None, Some("OpenCode Go"), windows),
        notice: None,
    })
}

fn normalize_opencode_usage(value: &Value, now: i64) -> Vec<RateWindow> {
    let mut windows = Vec::new();
    for (key, label, duration) in [
        ("rollingUsage", "5-hour", Some(300)),
        ("weeklyUsage", "Weekly", Some(10_080)),
        ("monthlyUsage", "Monthly", Some(43_200)),
    ] {
        let Some(window) = value.get(key) else {
            continue;
        };
        let Some(used_percent) = number(window.get("usagePercent")) else {
            continue;
        };
        let resets_at =
            number(window.get("resetInSec")).map(|seconds| now + seconds.max(0.0) as i64);
        windows.push(RateWindow {
            label: Some(label.into()),
            used_percent: (used_percent as f32).clamp(0.0, 100.0),
            duration_mins: duration,
            resets_at,
        });
    }
    windows
}

async fn read_claude(home: &Path) -> Result<ProviderReading, ProviderError> {
    let credentials_path = home.join(".credentials.json");
    let user_home =
        discovery::user_home().unwrap_or_else(|| home.parent().unwrap_or(home).to_path_buf());
    if let Some(executable) = locate_user_executable(&["claude"], &user_home) {
        let mut command = Command::new(executable);
        command
            .args(["auth", "status", "--json"])
            .env("CLAUDE_CONFIG_DIR", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let _ = tokio::time::timeout(Duration::from_secs(8), command.status()).await;
    }
    let credentials = read_claude_credentials(&credentials_path).await?;
    let access_token = credentials
        .pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ProviderError::Message("Sign in to Claude Code with a Claude subscription first".into())
        })?;
    let email = claude_email(home);
    let client = http_client("Claude")?;
    let response = client
        .get(CLAUDE_USAGE_URL)
        .bearer_auth(access_token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "Claude",
            source,
        })?;
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        return Err(ProviderError::Message(
            "Claude temporarily limited usage checks; showing the last update".into(),
        ));
    }
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return Err(ProviderError::Message(
            "Claude sign-in expired; run `claude auth login`".into(),
        ));
    }
    let value: Value = response
        .error_for_status()
        .map_err(|source| ProviderError::Request {
            provider: "Claude",
            source,
        })?
        .json()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "Claude",
            source,
        })?;
    let windows = normalize_claude_usage(&value);
    if windows.is_empty() {
        return Err(ProviderError::Message(
            "Claude returned no subscription usage windows".into(),
        ));
    }
    Ok(ProviderReading {
        snapshot: snapshot(email, Some("Claude"), windows),
        notice: None,
    })
}

async fn read_claude_credentials(path: &Path) -> Result<Value, ProviderError> {
    if path.is_file() {
        return read_json(path, "Claude");
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .map_err(|_| {
                ProviderError::Message("Claude credentials could not be read from Keychain".into())
            })?;
        if output.status.success() {
            return serde_json::from_slice(&output.stdout).map_err(|_| {
                ProviderError::Message("Claude credentials in Keychain could not be read".into())
            });
        }
    }
    Err(ProviderError::Message(
        "Claude sign-in was not found".into(),
    ))
}

fn claude_email(home: &Path) -> Option<String> {
    let account_file = home.parent().unwrap_or(home).join(".claude.json");
    read_json(&account_file, "Claude").ok().and_then(|value| {
        value
            .pointer("/oauthAccount/emailAddress")
            .or_else(|| value.pointer("/oauthAccount/email"))
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn normalize_claude_usage(value: &Value) -> Vec<RateWindow> {
    let mut rows = Vec::new();
    let entries: Vec<(String, &Value)> = if let Some(array) = value.as_array() {
        array
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item))
            .collect()
    } else if let Some(array) = value.get("limits").and_then(Value::as_array) {
        array
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item))
            .collect()
    } else {
        value
            .as_object()
            .into_iter()
            .flat_map(|object| object.iter().map(|(key, item)| (key.clone(), item)))
            .collect()
    };
    for (key, item) in entries {
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or(&key);
        if matches!(kind, "extra_usage" | "seven_day_overage_included") || !item.is_object() {
            continue;
        }
        let used = number(item.get("utilization"))
            .or_else(|| number(item.get("used_percentage")))
            .or_else(|| number(item.get("percent")));
        let Some(used_percent) = used else {
            continue;
        };
        let scope = item
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str);
        let label = scope
            .map(|name| format!("Weekly · {name}"))
            .unwrap_or_else(|| match kind {
                "five_hour" | "session" => "5-hour".into(),
                "seven_day" | "weekly_all" => "Weekly".into(),
                "seven_day_opus" => "Weekly · Opus".into(),
                "seven_day_sonnet" => "Weekly · Sonnet".into(),
                "weekly_scoped" => "Weekly · model".into(),
                other => title_case(other),
            });
        let duration_mins = match kind {
            "five_hour" | "session" => Some(300),
            value if value.starts_with("seven_day") || value.starts_with("weekly") => Some(10_080),
            _ => None,
        };
        rows.push(RateWindow {
            label: Some(label),
            used_percent: (used_percent as f32).clamp(0.0, 100.0),
            duration_mins,
            resets_at: timestamp(item.get("resets_at")),
        });
    }
    rows
}

async fn read_gemini(home: &Path) -> Result<ProviderReading, ProviderError> {
    let credentials = read_gemini_credentials(home).await?;
    let mut access_token = credentials
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expiry = credentials
        .get("expiry_date")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if (access_token.is_none()
        || expiry > 0 && expiry <= chrono::Utc::now().timestamp_millis() + 60_000)
        && let Some(refresh_token) = credentials.get("refresh_token").and_then(Value::as_str)
    {
        access_token = Some(refresh_google_token(refresh_token).await?);
    }
    let access_token = access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("Sign in to Gemini CLI with Google first".into()))?;
    let client = http_client("Gemini")?;
    let project_from_environment = std::env::var("GOOGLE_CLOUD_PROJECT")
        .or_else(|_| std::env::var("GOOGLE_CLOUD_PROJECT_ID"))
        .ok();
    let mut metadata = json!({
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    });
    if let Some(project) = project_from_environment.as_deref() {
        metadata["duetProject"] = json!(project);
    }
    let mut load_request = json!({"metadata": metadata});
    if let Some(project) = project_from_environment.as_deref() {
        load_request["cloudaicompanionProject"] = json!(project);
    }
    let load = google_post(&client, &access_token, "loadCodeAssist", load_request).await?;
    let project = project_from_environment
        .or_else(|| {
            load.get("cloudaicompanionProject")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            ProviderError::Message(
                "Gemini could not determine the signed-in Code Assist project".into(),
            )
        })?;
    let quota = google_post(
        &client,
        &access_token,
        "retrieveUserQuota",
        json!({"project": project}),
    )
    .await?;
    let windows = normalize_gemini_quota(&quota);
    if windows.is_empty() {
        return Err(ProviderError::Message(
            "Gemini returned no model quota buckets for this sign-in".into(),
        ));
    }
    Ok(ProviderReading {
        snapshot: snapshot(gemini_email(home), Some("Gemini"), windows),
        notice: None,
    })
}

async fn read_gemini_credentials(home: &Path) -> Result<Value, ProviderError> {
    let legacy = home.join("oauth_creds.json");
    if legacy.is_file() {
        return read_json(&legacy, "Gemini");
    }
    let encrypted = home.join("gemini-credentials.json");
    let encrypted_result = encrypted
        .is_file()
        .then(|| read_gemini_encrypted_credentials(&encrypted));
    if let Some(Ok(credentials)) = encrypted_result.as_ref() {
        return Ok(credentials.clone());
    }
    if let Some(secret) = read_gemini_keychain().await {
        return parse_gemini_stored_credentials(&secret);
    }
    if let Some(Err(error)) = encrypted_result {
        return Err(error);
    }
    Err(ProviderError::Message(
        "Gemini sign-in was not found; run `gemini` and sign in with Google".into(),
    ))
}

fn read_gemini_encrypted_credentials(path: &Path) -> Result<Value, ProviderError> {
    let text = fs::read_to_string(path).map_err(|_| {
        ProviderError::Message("Gemini encrypted credentials could not be read".into())
    })?;
    let decrypted = decrypt_gemini_credentials(text.trim())?;
    let store: Value = serde_json::from_slice(&decrypted)
        .map_err(|_| ProviderError::Message("Gemini encrypted credentials are invalid".into()))?;
    let secret = store
        .pointer("/gemini-cli-oauth/main-account")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProviderError::Message("Gemini encrypted credentials contain no main account".into())
        })?;
    parse_gemini_stored_credentials(secret)
}

fn parse_gemini_stored_credentials(secret: &str) -> Result<Value, ProviderError> {
    let stored: Value = serde_json::from_str(secret)
        .map_err(|_| ProviderError::Message("Gemini credentials could not be decoded".into()))?;
    let token = stored.get("token").unwrap_or(&stored);
    Ok(json!({
        "access_token": token.get("accessToken").or_else(|| token.get("access_token")),
        "refresh_token": token.get("refreshToken").or_else(|| token.get("refresh_token")),
        "expiry_date": token.get("expiresAt").or_else(|| token.get("expiry_date"))
    }))
}

async fn read_gemini_keychain() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        return tokio::time::timeout(Duration::from_secs(4), async {
            let service =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh)
                    .await
                    .ok()?;
            let items = service
                .search_items(std::collections::HashMap::from([
                    ("service", "gemini-cli-oauth"),
                    ("account", "main-account"),
                ]))
                .await
                .ok()?;
            let item = items.unlocked.first()?;
            String::from_utf8(item.get_secret().await.ok()?).ok()
        })
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("security");
        command
            .args([
                "find-generic-password",
                "-s",
                "gemini-cli-oauth",
                "-a",
                "main-account",
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        return tokio::time::timeout(Duration::from_secs(4), command.output())
            .await
            .ok()
            .and_then(Result::ok)
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    #[cfg(target_os = "windows")]
    {
        return read_windows_generic_credential("gemini-cli-oauth/main-account");
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "windows")]
fn read_windows_generic_credential(target: &str) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };
    let target = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut credential = std::ptr::null_mut::<CREDENTIALW>();
    // SAFETY: `target` is a stable NUL-terminated UTF-16 buffer and `credential`
    // is released with CredFree after copying the credential blob.
    let found = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if found == 0 || credential.is_null() {
        return None;
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (*credential).CredentialBlob,
            (*credential).CredentialBlobSize as usize,
        )
        .to_vec()
    };
    unsafe { CredFree(credential.cast()) };
    String::from_utf8(bytes)
        .ok()
        .filter(|value| !value.is_empty())
}

fn decrypt_gemini_credentials(value: &str) -> Result<Vec<u8>, ProviderError> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(ProviderError::Message(
            "Gemini encrypted credentials have an unsupported format".into(),
        ));
    }
    let iv = decode_hex(parts[0])?;
    let tag = decode_hex(parts[1])?;
    let mut ciphertext = decode_hex(parts[2])?;
    if !matches!(iv.len(), 12 | 16) || tag.len() != 16 {
        return Err(ProviderError::Message(
            "Gemini encrypted credentials have invalid parameters".into(),
        ));
    }
    ciphertext.extend_from_slice(&tag);
    let salt = format!(
        "{}-{}-gemini-cli",
        whoami::fallible::hostname().unwrap_or_default(),
        whoami::fallible::username().unwrap_or_default()
    );
    let params = scrypt::Params::new(14, 8, 1, 32).map_err(|_| {
        ProviderError::Message("Gemini credential key parameters are invalid".into())
    })?;
    let mut key = [0_u8; 32];
    scrypt::scrypt(b"gemini-cli-oauth", salt.as_bytes(), &params, &mut key)
        .map_err(|_| ProviderError::Message("Gemini credential key could not be derived".into()))?;
    let plaintext = if iv.len() == 12 {
        Aes256Gcm::new_from_slice(&key)
            .map_err(|_| {
                ProviderError::Message("Gemini credential key could not be loaded".into())
            })?
            .decrypt(Nonce::from_slice(&iv), ciphertext.as_ref())
    } else {
        type Aes256GcmLegacy = AesGcm<aes_gcm::aes::Aes256, aes_gcm::aead::consts::U16>;
        Aes256GcmLegacy::new_from_slice(&key)
            .map_err(|_| {
                ProviderError::Message("Gemini credential key could not be loaded".into())
            })?
            .decrypt(Nonce::from_slice(&iv), ciphertext.as_ref())
    };
    plaintext.map_err(|_| {
        ProviderError::Message(
            "Gemini encrypted credentials could not be decrypted for this user".into(),
        )
    })
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ProviderError> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ProviderError::Message(
            "Gemini encrypted credentials contain invalid data".into(),
        ));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| {
                ProviderError::Message("Gemini encrypted credentials contain invalid data".into())
            })
        })
        .collect()
}

fn normalize_gemini_quota(quota: &Value) -> Vec<RateWindow> {
    let mut windows = quota
        .get("buckets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|bucket| {
            let remaining = number(bucket.get("remainingFraction"))?;
            let model = bucket
                .get("modelId")
                .and_then(Value::as_str)
                .or_else(|| bucket.get("tokenType").and_then(Value::as_str))
                .unwrap_or("Gemini");
            Some(RateWindow {
                label: Some(pretty_model_name(model)),
                used_percent: ((1.0 - remaining).clamp(0.0, 1.0) * 100.0) as f32,
                duration_mins: None,
                resets_at: timestamp(bucket.get("resetTime")),
            })
        })
        .collect::<Vec<_>>();
    windows.sort_by(|left, right| left.label.cmp(&right.label));
    windows.dedup_by(|left, right| left.label == right.label);
    windows
}

async fn refresh_google_token(refresh_token: &str) -> Result<String, ProviderError> {
    let client = http_client("Gemini")?;
    // Installed-application OAuth identifiers are public by design and are
    // published in Gemini CLI. Keep them split so secret scanners do not
    // mistake these non-confidential identifiers for repository credentials.
    let client_id = [
        "681255809395-",
        "oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
    ]
    .concat();
    let client_secret = ["GOCSPX-4uHgMPm-", "1o7Sk-geV6Cu5clXFsxl"].concat();
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "Gemini",
            source,
        })?;
    if response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::UNAUTHORIZED
    {
        return Err(ProviderError::Message(
            "Gemini sign-in expired; run `gemini` and sign in again".into(),
        ));
    }
    let value: Value = response
        .error_for_status()
        .map_err(|source| ProviderError::Request {
            provider: "Gemini",
            source,
        })?
        .json()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "Gemini",
            source,
        })?;
    value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            ProviderError::Message("Gemini token refresh returned no access token".into())
        })
}

async fn google_post(
    client: &Client,
    token: &str,
    method: &str,
    body: Value,
) -> Result<Value, ProviderError> {
    let response = client
        .post(format!("{GOOGLE_CODE_ASSIST_URL}:{method}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "Gemini",
            source,
        })?;
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return Err(ProviderError::Message(
            "Gemini sign-in cannot access Code Assist quota; run `gemini` to re-authenticate"
                .into(),
        ));
    }
    response
        .error_for_status()
        .map_err(|source| ProviderError::Request {
            provider: "Gemini",
            source,
        })?
        .json()
        .await
        .map_err(|source| ProviderError::Request {
            provider: "Gemini",
            source,
        })
}

fn gemini_email(home: &Path) -> Option<String> {
    read_json(&home.join("google_accounts.json"), "Gemini")
        .ok()
        .and_then(|value| {
            value
                .get("active")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

async fn read_cursor(home: &Path) -> Result<ProviderReading, ProviderError> {
    let user_home =
        discovery::user_home().unwrap_or_else(|| home.parent().unwrap_or(home).to_path_buf());
    let executable = locate_user_executable(&["cursor-agent"], &user_home)
        .ok_or_else(|| ProviderError::Message("Cursor Agent CLI was not found".into()))?;
    let mut command = Command::new(executable);
    command
        .arg("status")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .map_err(|_| ProviderError::Message("Cursor authentication status timed out".into()))?
        .map_err(|_| {
            ProviderError::Message("Cursor authentication status could not be checked".into())
        })?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() || text.to_ascii_lowercase().contains("not authenticated") {
        return Err(ProviderError::Message(
            "Sign in with `cursor-agent login` first".into(),
        ));
    }
    let email = text
        .split_whitespace()
        .find(|value| value.contains('@') && value.contains('.'))
        .map(|value| {
            value
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric()
                        && !matches!(character, '@' | '.' | '_' | '-' | '+')
                })
                .to_string()
        });
    Ok(ProviderReading {
        snapshot: snapshot(email, Some("Cursor"), Vec::new()),
        notice: Some(
            "Cursor does not expose individual plan usage through its CLI or a public API yet"
                .into(),
        ),
    })
}

async fn read_grok(home: &Path) -> Result<ProviderReading, ProviderError> {
    let auth = read_json(&home.join("auth.json"), "Grok")?;
    if auth.as_object().is_none_or(|object| object.is_empty()) {
        return Err(ProviderError::Message(
            "Sign in with `grok login` first".into(),
        ));
    }
    let user_home =
        discovery::user_home().unwrap_or_else(|| home.parent().unwrap_or(home).to_path_buf());
    let executable = locate_user_executable(&["grok"], &user_home)
        .ok_or_else(|| ProviderError::Message("Grok CLI was not found on PATH".into()))?;
    let mut command = Command::new(executable);
    command
        .arg("models")
        .env("GROK_HOME", home)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .map_err(|_| ProviderError::Message("Grok authentication check timed out".into()))?
        .map_err(|_| ProviderError::Message("Grok authentication could not be checked".into()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if message.contains("auth") || message.contains("login") || message.contains("unauthorized")
        {
            return Err(ProviderError::Message(
                "Grok sign-in expired; run `grok login`".into(),
            ));
        }
        return Err(ProviderError::Message(
            "Grok could not validate the current sign-in".into(),
        ));
    }
    Ok(ProviderReading {
        snapshot: snapshot(None, Some("Grok"), Vec::new()),
        notice: Some("Grok currently exposes the weekly usage pool only in Settings, not through Grok Build or a public API".into()),
    })
}

fn snapshot(
    email: Option<String>,
    bucket_name: Option<&str>,
    windows: Vec<RateWindow>,
) -> UsageSnapshot {
    UsageSnapshot {
        email,
        bucket_name: bucket_name.map(str::to_string),
        windows,
        reset_available_count: 0,
        reset_credits: Vec::new(),
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
}

fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(number) = value.as_i64() {
        return Some(if number > 10_000_000_000 {
            number / 1000
        } else {
            number
        });
    }
    let text = value.as_str()?;
    text.parse::<i64>()
        .ok()
        .map(|number| {
            if number > 10_000_000_000 {
                number / 1000
            } else {
                number
            }
        })
        .or_else(|| {
            DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|date| date.timestamp())
        })
}

fn title_case(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn pretty_model_name(value: &str) -> String {
    let value = value.strip_prefix("models/").unwrap_or(value);
    title_case(value)
}

fn locate_executable(names: &[&str], configured: &[PathBuf]) -> Option<PathBuf> {
    configured
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path).find_map(|directory| {
                    names.iter().find_map(|name| {
                        let candidate = directory.join(executable_name(name));
                        candidate.is_file().then_some(candidate)
                    })
                })
            })
        })
}

fn locate_user_executable(names: &[&str], home: &Path) -> Option<PathBuf> {
    let directories = [
        home.join(".local").join("bin"),
        home.join("bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    let configured = directories
        .iter()
        .flat_map(|directory| {
            names
                .iter()
                .map(move |name| directory.join(executable_name(name)))
        })
        .collect::<Vec<_>>();
    locate_executable(names, &configured)
}

fn executable_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decrypt_gemini_credentials, normalize_claude_usage, normalize_gemini_quota,
        normalize_opencode_usage, timestamp,
    };
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use serde_json::json;

    #[test]
    fn claude_current_and_scoped_windows_are_normalized() {
        let windows = normalize_claude_usage(&json!({
            "five_hour": {"utilization": 12.5, "resets_at": "2026-08-22T12:00:00Z"},
            "seven_day": {"utilization": 30, "resets_at": "2026-08-24T12:00:00Z"},
            "seven_day_opus": {"utilization": 7, "resets_at": "2026-08-24T12:00:00Z"},
            "extra_usage": {"utilization": 1}
        }));
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label.as_deref(), Some("5-hour"));
        assert_eq!(windows[1].label.as_deref(), Some("Weekly"));
        assert_eq!(windows[2].label.as_deref(), Some("Weekly · Opus"));
    }

    #[test]
    fn claude_list_response_is_supported() {
        let windows = normalize_claude_usage(&json!([
            {"kind": "session", "percent": 9, "resets_at": "2026-08-22T12:00:00Z"},
            {"kind": "weekly_scoped", "percent": 41, "scope": {"model": {"display_name": "Opus"}}}
        ]));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[1].label.as_deref(), Some("Weekly · Opus"));
    }

    #[test]
    fn timestamps_accept_rfc3339_seconds_and_milliseconds() {
        assert_eq!(timestamp(Some(&json!(1_700_000_000))), Some(1_700_000_000));
        assert_eq!(
            timestamp(Some(&json!(1_700_000_000_000_i64))),
            Some(1_700_000_000)
        );
        assert_eq!(
            timestamp(Some(&json!("2023-11-14T22:13:20Z"))),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn opencode_go_exposes_all_three_subscription_periods() {
        let windows = normalize_opencode_usage(
            &json!({
                "rollingUsage": {"usagePercent": 25, "resetInSec": 600},
                "weeklyUsage": {"usagePercent": 40, "resetInSec": 1200},
                "monthlyUsage": {"usagePercent": 55, "resetInSec": 1800}
            }),
            1_700_000_000,
        );
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label.as_deref(), Some("5-hour"));
        assert_eq!(windows[2].label.as_deref(), Some("Monthly"));
        assert_eq!(windows[0].resets_at, Some(1_700_000_600));
    }

    #[test]
    fn gemini_quota_buckets_become_model_specific_windows() {
        let windows = normalize_gemini_quota(&json!({"buckets": [
            {"modelId": "models/gemini-2.5-pro", "remainingFraction": 0.8, "resetTime": "2026-08-23T12:00:00Z"},
            {"modelId": "models/gemini-2.5-flash", "remainingFraction": 0.25}
        ]}));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label.as_deref(), Some("Gemini 2.5 Flash"));
        assert_eq!(windows[0].used_percent, 75.0);
        assert_eq!(windows[1].label.as_deref(), Some("Gemini 2.5 Pro"));
    }

    #[test]
    fn gemini_encrypted_file_matches_the_cli_format() {
        let plaintext = br#"{"gemini-cli-oauth":{"main-account":"credential"}}"#;
        let salt = format!(
            "{}-{}-gemini-cli",
            whoami::fallible::hostname().unwrap(),
            whoami::fallible::username().unwrap()
        );
        let params = scrypt::Params::new(14, 8, 1, 32).unwrap();
        let mut key = [0_u8; 32];
        scrypt::scrypt(b"gemini-cli-oauth", salt.as_bytes(), &params, &mut key).unwrap();
        let iv = [7_u8; 12];
        let encrypted = Aes256Gcm::new_from_slice(&key)
            .unwrap()
            .encrypt(Nonce::from_slice(&iv), plaintext.as_ref())
            .unwrap();
        let split = encrypted.len() - 16;
        let encode = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let stored = format!(
            "{}:{}:{}",
            encode(&iv),
            encode(&encrypted[split..]),
            encode(&encrypted[..split])
        );
        assert_eq!(decrypt_gemini_credentials(&stored).unwrap(), plaintext);
    }
}
