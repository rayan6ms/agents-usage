use crate::discovery::user_home;
use crate::domain::{RateWindow, ResetCredit, UsageSnapshot};
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const RPC_TIMEOUT: Duration = Duration::from_secs(8);
const READ_ATTEMPTS: usize = 3;
const READ_RETRY_BASE_MS: u64 = 350;

#[derive(Debug, Error)]
pub enum CodexError {
    #[error("Codex executable was not found")]
    NotFound,
    #[error("could not start Codex: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("Codex App Server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Codex App Server timed out waiting for response {0}")]
    Timeout(i64),
    #[error("Codex App Server exited before response {0}")]
    Exited(i64),
    #[error("Codex App Server returned an error for {method}: {message}")]
    Rpc { method: String, message: String },
    #[error("Codex App Server returned an unexpected response: {0}")]
    Protocol(String),
}

impl CodexError {
    pub fn is_transient_read(&self) -> bool {
        match self {
            // A short-lived App Server child can race with transient local/transport
            // failures. Reads are safe to retry because they have no side effects.
            Self::Io(_) | Self::Timeout(_) | Self::Exited(_) => true,
            Self::Rpc { message, .. } => {
                let lower = message.to_ascii_lowercase();
                [
                    "503 service unavailable",
                    "502 bad gateway",
                    "504 gateway timeout",
                    "server overloaded",
                    "retry later",
                    "upstream connect error",
                    "connection termination",
                    "connection reset",
                    "temporarily unavailable",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
            }
            Self::NotFound | Self::Spawn(_) | Self::Protocol(_) => false,
        }
    }

    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Rpc { message, .. } => {
                let lower = message.to_ascii_lowercase();
                if lower.contains("401 unauthorized")
                    || lower.contains("token_revoked")
                    || lower.contains("invalidated oauth token")
                {
                    "Sign-in expired · log in to this account again"
                } else if lower.contains("429 too many requests") || lower.contains("rate limit") {
                    "Service rate-limited · try again shortly"
                } else if self.is_transient_read() {
                    "Temporary service error · keeping last data"
                } else {
                    "Account service rejected the refresh"
                }
            }
            Self::Io(_) | Self::Timeout(_) | Self::Exited(_) => {
                "Temporary service error · keeping last data"
            }
            Self::NotFound | Self::Spawn(_) => "Codex could not be started for this account",
            Self::Protocol(_) => "Usage data was not available for this account",
        }
    }
}

fn retry_delay(codex_home: &Path, attempt: usize) -> Duration {
    let mut hasher = DefaultHasher::new();
    codex_home.hash(&mut hasher);
    attempt.hash(&mut hasher);
    let jitter = hasher.finish() % 151;
    let exponential = READ_RETRY_BASE_MS.saturating_mul(1u64 << attempt.saturating_sub(1));
    Duration::from_millis(exponential + jitter)
}

pub fn locate_codex(configured: Option<&Path>) -> Result<PathBuf, CodexError> {
    let mut candidates = Vec::new();
    if let Some(value) = std::env::var_os("AGENTS_USAGE_CODEX_BIN") {
        candidates.push(PathBuf::from(value));
    }
    if let Some(path) = configured {
        candidates.push(path.to_path_buf());
    }
    for name in executable_names() {
        if let Some(path) = find_on_path(name) {
            candidates.push(path);
        }
    }
    if let Some(home) = user_home() {
        for name in executable_names() {
            candidates.push(home.join(".local").join("bin").join(name));
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let bin = PathBuf::from(local).join("Programs").join("OpenAI").join("Codex").join("bin");
        candidates.extend(executable_names().iter().map(|name| bin.join(name)));
    }
    #[cfg(target_os = "windows")]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        let npm = PathBuf::from(app_data).join("npm");
        candidates.extend(executable_names().iter().map(|name| npm.join(name)));
    }
    for directory in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        candidates.extend(executable_names().iter().map(|name| PathBuf::from(directory).join(name)));
    }

    candidates
        .into_iter()
        .find(|path| is_executable_file(path))
        .ok_or(CodexError::NotFound)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn executable_names() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    { &["codex.exe", "codex.cmd", "codex.bat"] }
    #[cfg(not(target_os = "windows"))]
    { &["codex"] }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

pub async fn read_openai_account(codex: &Path, codex_home: &Path) -> Result<UsageSnapshot, CodexError> {
    let mut last_error = None;
    for attempt in 1..=READ_ATTEMPTS {
        match read_openai_account_once(codex, codex_home).await {
            Ok(snapshot) => {
                if attempt > 1 {
                    eprintln!(
                        "Codex read recovered: {} succeeded on attempt {attempt}/{READ_ATTEMPTS}",
                        codex_home.display()
                    );
                }
                return Ok(snapshot);
            }
            Err(error) => {
                let retryable = error.is_transient_read();
                if !retryable || attempt == READ_ATTEMPTS {
                    return Err(error);
                }
                let delay = retry_delay(codex_home, attempt);
                eprintln!(
                    "Codex read transient failure: {} attempt {attempt}/{READ_ATTEMPTS}: {error}; retrying in {}ms",
                    codex_home.display(),
                    delay.as_millis(),
                );
                last_error = Some(error);
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(last_error.expect("read retry loop always records an error before exhaustion"))
}

pub async fn read_openai_identity(
    codex: &Path,
    codex_home: &Path,
) -> Result<Option<String>, CodexError> {
    let mut session = AppServerSession::start(codex, codex_home).await?;
    let result = async {
        session.initialize().await?;
        let account = session
            .request(1, "account/read", Some(json!({"refreshToken": false})))
            .await?;
        Ok(account_email(&account))
    }
    .await;
    session.shutdown().await;
    result
}

async fn read_openai_account_once(codex: &Path, codex_home: &Path) -> Result<UsageSnapshot, CodexError> {
    let mut session = AppServerSession::start(codex, codex_home).await?;
    let result = async {
        session.initialize().await?;
        let account = session
            .request(1, "account/read", Some(json!({"refreshToken": false})))
            .await?;
        let limits = session
            .request(2, "account/rateLimits/read", None)
            .await?;
        normalize_snapshot(account, limits)
    }
    .await;
    session.shutdown().await;
    result
}

pub async fn consume_reset(
    codex: &Path,
    codex_home: &Path,
    idempotency_key: &str,
    credit_id: Option<&str>,
) -> Result<String, CodexError> {
    let mut session = AppServerSession::start(codex, codex_home).await?;
    let result = async {
        session.initialize().await?;
        let mut params = serde_json::Map::new();
        params.insert("idempotencyKey".into(), Value::String(idempotency_key.to_string()));
        if let Some(credit_id) = credit_id.filter(|value| !value.is_empty()) {
            params.insert("creditId".into(), Value::String(credit_id.to_string()));
        }
        let value = session
            .request(8, "account/rateLimitResetCredit/consume", Some(Value::Object(params)))
            .await?;
        value
            .get("outcome")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CodexError::Protocol("reset response is missing outcome".into()))
    }
    .await;
    session.shutdown().await;
    result
}

struct AppServerSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl AppServerSession {
    async fn start(codex: &Path, codex_home: &Path) -> Result<Self, CodexError> {
        let mut command = Command::new(codex);
        command
            .arg("app-server")
            .env("CODEX_HOME", codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        {
            // Keep short-lived CLI children from flashing console windows out
            // of the tray application. CREATE_NO_WINDOW is 0x08000000.
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn()
            .map_err(CodexError::Spawn)?;
        let stdin = child.stdin.take().ok_or_else(|| CodexError::Protocol("missing App Server stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| CodexError::Protocol("missing App Server stdout".into()))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout).lines(),
        })
    }

    async fn initialize(&mut self) -> Result<(), CodexError> {
        self.send(&json!({
            "method": "initialize",
            "id": 0,
            "params": {
                "clientInfo": {
                    "name": "agents-usage",
                    "title": "Agents Usage",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": false }
            }
        }))
        .await?;
        let _ = self.wait_for_id(0, "initialize").await?;
        self.send(&json!({"method": "initialized", "params": {}})).await
    }

    async fn request(
        &mut self,
        id: i64,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, CodexError> {
        let mut request = serde_json::Map::new();
        request.insert("method".into(), Value::String(method.to_string()));
        request.insert("id".into(), Value::Number(id.into()));
        if let Some(params) = params {
            request.insert("params".into(), params);
        }
        self.send(&Value::Object(request)).await?;
        self.wait_for_id(id, method).await
    }

    async fn send(&mut self, value: &Value) -> Result<(), CodexError> {
        let mut encoded = serde_json::to_vec(value)
            .map_err(|error| CodexError::Protocol(error.to_string()))?;
        encoded.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or_else(|| CodexError::Protocol("App Server stdin is closed".into()))?;
        stdin.write_all(&encoded).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn wait_for_id(&mut self, id: i64, method: &str) -> Result<Value, CodexError> {
        let future = async {
            loop {
                let line = self.stdout.next_line().await?;
                let Some(line) = line else {
                    return Err(CodexError::Exited(id));
                };
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("id").and_then(Value::as_i64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    let message_text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown App Server error")
                        .to_string();
                    return Err(CodexError::Rpc {
                        method: method.to_string(),
                        message: message_text,
                    });
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| CodexError::Protocol(format!("{method} response is missing result")));
            }
        };
        timeout(RPC_TIMEOUT, future)
            .await
            .map_err(|_| CodexError::Timeout(id))?
    }

    async fn shutdown(&mut self) {
        self.stdin.take();
        match timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = self.child.start_kill();
                let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
            }
        }
    }
}

fn normalize_snapshot(account: Value, limits: Value) -> Result<UsageSnapshot, CodexError> {
    let email = account_email(&account);
    let (bucket_name, windows) = normalize_windows(&limits);
    if windows.is_empty() {
        return Err(CodexError::Protocol("account/rateLimits/read returned no usable windows".into()));
    }

    let (reset_available_count, reset_credits) = normalize_reset_credits(&limits);
    Ok(UsageSnapshot {
        email,
        bucket_name,
        windows,
        reset_available_count,
        reset_credits,
    })
}

fn account_email(account: &Value) -> Option<String> {
    account
        .get("account")
        .and_then(Value::as_object)
        .and_then(|value| value.get("email"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn normalize_windows(result: &Value) -> (Option<String>, Vec<RateWindow>) {
    let bucket = result
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
        .and_then(|map| {
            map.get("codex")
                .or_else(|| map.iter().min_by_key(|(key, _)| *key).map(|(_, value)| value))
        })
        .or_else(|| result.get("rateLimits"));

    let Some(bucket) = bucket else { return (None, Vec::new()); };
    let name = bucket.get("limitName").and_then(Value::as_str).map(str::to_string);
    let mut windows = Vec::new();
    for field in ["primary", "secondary"] {
        let Some(value) = bucket.get(field).and_then(Value::as_object) else { continue; };
        let Some(used) = value.get("usedPercent").and_then(Value::as_f64) else { continue; };
        windows.push(RateWindow {
            used_percent: (used as f32).clamp(0.0, 100.0),
            duration_mins: value.get("windowDurationMins").and_then(Value::as_u64),
            resets_at: value.get("resetsAt").and_then(Value::as_i64),
        });
    }
    windows.sort_by_key(|window| window.duration_mins.unwrap_or(u64::MAX));
    (name, windows)
}

fn normalize_reset_credits(result: &Value) -> (u32, Vec<ResetCredit>) {
    let Some(value) = result.get("rateLimitResetCredits").and_then(Value::as_object) else {
        return (0, Vec::new());
    };
    let count = value
        .get("availableCount")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let credits = value
        .get("credits")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let object = item.as_object()?;
                    Some(ResetCredit {
                        id: object.get("id").and_then(Value::as_str).map(str::to_string),
                        title: object.get("title").and_then(Value::as_str).map(str::to_string),
                        description: object.get("description").and_then(Value::as_str).map(str::to_string),
                        expires_at: object.get("expiresAt").and_then(Value::as_i64),
                        status: object.get("status").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (count, credits)
}

#[cfg(test)]
mod tests {
    use super::{account_email, CodexError};
    use serde_json::json;

    fn rpc(message: &str) -> CodexError {
        CodexError::Rpc {
            method: "account/rateLimits/read".into(),
            message: message.into(),
        }
    }

    #[test]
    fn authentication_errors_are_actionable_and_not_retried() {
        let error = rpc("401 Unauthorized: token_revoked: invalidated oauth token");
        assert!(!error.is_transient_read());
        assert_eq!(error.user_message(), "Sign-in expired · log in to this account again");
    }

    #[test]
    fn rate_limits_are_actionable_and_not_amplified_by_retries() {
        let error = rpc("429 Too Many Requests: rate limit exceeded");
        assert!(!error.is_transient_read());
        assert_eq!(error.user_message(), "Service rate-limited · try again shortly");
    }

    #[test]
    fn temporary_upstream_errors_keep_the_retry_path() {
        let error = rpc("503 Service Unavailable: retry later");
        assert!(error.is_transient_read());
        assert_eq!(error.user_message(), "Temporary service error · keeping last data");
    }

    #[test]
    fn account_identity_is_available_without_a_usage_response() {
        let account = json!({
            "account": {
                "type": "chatgpt",
                "email": "Moved.Account@Example.com",
                "planType": "plus"
            }
        });

        assert_eq!(
            account_email(&account).as_deref(),
            Some("Moved.Account@Example.com")
        );
    }
}
