use crate::config::{self, AppConfig};
use crate::discovery;
use crate::domain::{RateWindow, UsageSnapshot};
use crate::providers::{ProviderCandidate, ProviderError, ProviderReading};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

pub const ID: &str = "antigravity";
pub const IMPORT_COMMAND: &str =
    "agy -p /usage --output-format json | agents-usage --import-antigravity-usage";
const PROFILE_DIR: &str = "antigravity-acp";
const TOKEN_FILE: &str = "antigravity-oauth-token";
const SETTINGS_FILE: &str = "settings.json";
const MAX_REPORT_BYTES: u64 = 1024 * 1024;
const MAX_PROFILE_FILE_BYTES: u64 = 64 * 1024;
const CLI_REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const ACP_TIER_TIMEOUT: Duration = Duration::from_secs(6);

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

#[derive(Debug, PartialEq, Eq)]
enum CliRefreshError {
    NotFound,
    AuthRequired,
    Failed(String),
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

pub fn is_third_party_window(window: &RateWindow) -> bool {
    window.label.as_deref().is_some_and(|label| {
        label.starts_with("Claude/GPT") || label.starts_with("Claude & GPT")
    })
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

    /// Parse T3 Code's non-secret provider status cache (`~/.t3/caches/*.json`)
    /// when it holds a live `usageLimits` snapshot for the Antigravity driver.
    fn parse_t3_cache(bytes: &[u8]) -> Option<Self> {
        let root: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).ok()?;
        if root.get("driver").and_then(Value::as_str) != Some(ID) {
            return None;
        }
        let limits = root.get("usageLimits")?.as_object()?;
        if limits.get("unavailable").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let checked_at = limits
            .get("checkedAt")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|time| time.timestamp())?;
        let raw_windows = limits.get("windows").and_then(Value::as_array)?;
        let mut windows = Vec::new();
        for item in raw_windows {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some((_, duration, order)) = window_kind(id) else {
                continue;
            };
            let Some(used_percent) = item
                .get("usedPercent")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
            else {
                continue;
            };
            if windows
                .iter()
                .any(|(_, window): &(u8, QuotaWindow)| window.id == id)
            {
                continue;
            }
            let remaining = (1.0 - used_percent / 100.0).clamp(0.0, 1.0);
            let reset_at = item
                .get("resetsAt")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|time| time.timestamp())
                .filter(|time| {
                    !(remaining == 1.0
                        && time.abs_diff(checked_at.saturating_add(duration as i64 * 60)) <= 60)
                });
            windows.push((
                order,
                QuotaWindow {
                    id: id.into(),
                    remaining: remaining as f32,
                    reset_at,
                },
            ));
        }
        if windows.is_empty() {
            return None;
        }
        windows.sort_by_key(|(order, _)| *order);
        Some(Self {
            windows: windows.into_iter().map(|(_, window)| window).collect(),
            email: None,
            plan: None,
            saved_at: checked_at,
        })
    }

    fn with_fallback_metadata(mut self, fallback: Option<&Self>) -> Self {
        if let Some(other) = fallback {
            if self.email.is_none() {
                self.email.clone_from(&other.email);
            }
            if self.plan.is_none() {
                self.plan.clone_from(&other.plan);
            }
        }
        self
    }

    fn pick_fresher(primary: Option<Self>, secondary: Option<Self>) -> Option<Self> {
        match (primary, secondary) {
            (Some(first), Some(second)) => {
                if second.saved_at > first.saved_at {
                    Some(second.with_fallback_metadata(Some(&first)))
                } else {
                    Some(first.with_fallback_metadata(Some(&second)))
                }
            }
            (Some(first), None) => Some(first),
            (None, Some(second)) => Some(second),
            (None, None) => None,
        }
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
                    "Updated {}",
                    time.with_timezone(&chrono::Local).format("%b %e at %H:%M")
                )
            })
            .unwrap_or_else(|| "Updated quota report".into())
    }
}

pub fn manual_home() -> PathBuf {
    config::config_path().with_file_name("manual-antigravity")
}

pub fn is_manual_home(home: &Path) -> bool {
    let manual = manual_home();
    let a = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let b = manual.canonicalize().unwrap_or(manual);
    a == b
}

#[derive(Deserialize)]
struct ProfileTokenMetadata {
    auth_method: String,
    id_token_email: String,
}

fn read_profile_file(path: &Path) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_PROFILE_FILE_BYTES).then_some(bytes)
}

fn profile_metadata(profile: &Path) -> Option<ProfileTokenMetadata> {
    let dir = profile.join(PROFILE_DIR);
    let settings: Value = serde_json::from_slice(&read_profile_file(&dir.join(SETTINGS_FILE))?).ok()?;
    if settings.pointer("/auth/type")?.as_str()? != "oauth-personal" {
        return None;
    }
    let metadata: ProfileTokenMetadata =
        serde_json::from_slice(&read_profile_file(&dir.join(TOKEN_FILE))?).ok()?;
    let email = metadata.id_token_email.trim();
    if metadata.auth_method != "consumer"
        || email.is_empty()
        || email.chars().count() > 254
        || email.chars().any(char::is_control)
    {
        return None;
    }
    Some(ProfileTokenMetadata {
        auth_method: metadata.auth_method,
        id_token_email: email.to_owned(),
    })
}

pub fn local_email(profile: &Path) -> Option<String> {
    profile_metadata(profile).map(|meta| meta.id_token_email)
}

pub fn local_identity(profile: &Path) -> Option<(String, i64)> {
    let email = local_email(profile)?.to_ascii_lowercase();
    let mtime = fs::metadata(profile.join(PROFILE_DIR).join(TOKEN_FILE))
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    Some((email, mtime))
}

pub fn is_t3_profile(profile: &Path) -> bool {
    local_email(profile).is_some()
}

fn t3_root(home: &Path) -> PathBuf {
    std::env::var_os("T3CODE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".t3"))
}

pub fn t3_candidates(home: &Path) -> Vec<ProviderCandidate> {
    let Ok(profiles) = fs::read_dir(t3_root(home).join("userdata/providers/antigravity")) else {
        return Vec::new();
    };
    let mut homes: Vec<_> = profiles
        .flatten()
        .map(|entry| entry.path())
        .filter(|profile| is_t3_profile(profile))
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

pub fn t3_profile_homes() -> Vec<PathBuf> {
    discovery::user_home()
        .map(|home| t3_candidates(&home).into_iter().map(|c| c.home).collect())
        .unwrap_or_default()
}

pub fn has_t3_profiles() -> bool {
    !t3_profile_homes().is_empty()
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
    let executable_name = if cfg!(windows) {
        "agy.exe"
    } else {
        "antigravity"
    };
    if let Some(active) = read_profile_file(&runtime.join("active.json"))
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        && let Some(release) = active.get("releaseId").and_then(Value::as_str)
        && release.len() == 64
        && release.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        let executable = runtime.join("versions").join(release).join(executable_name);
        if is_executable_file(&executable) {
            return Some(executable);
        }
    }
    None
}

struct IsolatedCliHome(PathBuf);

impl IsolatedCliHome {
    fn for_profile(profile: &Path) -> io::Result<Self> {
        let token_source = profile.join(PROFILE_DIR).join(TOKEN_FILE);
        if !token_source.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Antigravity profile token file not found",
            ));
        }
        let root = std::env::temp_dir().join(format!("agents-usage-agy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
        }
        let guard = Self(root);
        let cli_dir = guard.0.join(".gemini").join("antigravity-cli");
        fs::create_dir_all(&cli_dir)?;
        let token_link = cli_dir.join(TOKEN_FILE);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&token_source, &token_link)?;
        #[cfg(target_os = "windows")]
        std::os::windows::fs::symlink_file(&token_source, &token_link)
            .or_else(|_| fs::hard_link(&token_source, &token_link))?;
        Ok(guard)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for IsolatedCliHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn read_t3_cache_for_profile(profile: &Path) -> Option<SavedQuota> {
    let profile_hash = profile.file_name()?.to_str()?.to_ascii_lowercase();
    let mut newest = None;
    for dir in t3_cache_dirs() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_REPORT_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(root) = serde_json::from_slice::<Value>(
                bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes),
            ) else {
                continue;
            };
            let Some(instance_id) = root.get("instanceId").and_then(Value::as_str) else {
                continue;
            };
            if sha256_hex(instance_id) != profile_hash {
                continue;
            }
            if let Some(parsed) = SavedQuota::parse_t3_cache(&bytes) {
                newest = SavedQuota::pick_fresher(newest, Some(parsed));
            }
        }
    }
    newest
}

fn report_path() -> PathBuf {
    config::config_path().with_file_name("antigravity-usage.json")
}

fn write_report(report: &SavedQuota) -> Result<(), String> {
    let data =
        serde_json::to_vec(report).map_err(|_| "Could not encode the Antigravity report.")?;
    config::atomic_write(&report_path(), &data)
        .map_err(|error| format!("Could not save the Antigravity report: {error}"))
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

fn t3_cache_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(t3_home) = std::env::var_os("T3CODE_HOME").filter(|value| !value.is_empty()) {
        dirs.push(PathBuf::from(t3_home).join("caches"));
    }
    if let Some(home) = discovery::user_home() {
        let default_dir = home.join(".t3").join("caches");
        if !dirs.contains(&default_dir) {
            dirs.push(default_dir);
        }
    }
    dirs
}

fn read_t3_cache_in_dir(dir: &Path) -> Option<SavedQuota> {
    let entries = fs::read_dir(dir).ok()?;
    let mut newest: Option<SavedQuota> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_REPORT_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if let Some(parsed) = SavedQuota::parse_t3_cache(&bytes) {
            newest = SavedQuota::pick_fresher(newest, Some(parsed));
        }
    }
    newest
}

pub fn read_t3_cache() -> Option<SavedQuota> {
    let mut newest = None;
    for dir in t3_cache_dirs() {
        newest = SavedQuota::pick_fresher(newest, read_t3_cache_in_dir(&dir));
    }
    newest
}

/// Return the freshest available local Antigravity reading across the saved
/// report file and T3 Code's non-secret provider status cache.
pub fn read_effective_report() -> Option<SavedQuota> {
    SavedQuota::pick_fresher(read_report(), read_t3_cache())
}

pub fn has_live_source() -> bool {
    locate_agy_executable().is_some() || read_t3_cache().is_some()
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
    let previous = read_report();
    let (mut report, status_line) =
        SavedQuota::parse(&bytes, chrono::Utc::now().timestamp()).map_err(str::to_owned)?;
    report = report.with_fallback_metadata(previous.as_ref());
    // The official status hook may repeat a payload frequently while typing.
    // Avoid rewriting an unchanged snapshot or making it look freshly fetched.
    if status_line && let Some(previous) = previous {
        let received_at = report.saved_at;
        report.saved_at = previous.saved_at;
        if report == previous {
            return Ok(());
        }
        report.saved_at = received_at;
    }
    write_report(&report)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn locate_agy_executable() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let names: &[&str] = &["agy.exe", "agy.cmd", "agy.bat"];
    #[cfg(not(target_os = "windows"))]
    let names: &[&str] = &["agy"];

    if let Some(path_env) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_env) {
            for name in names {
                let candidate = directory.join(name);
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    if let Some(home) = discovery::user_home() {
        let fallbacks = [
            home.join(".local").join("bin"),
            home.join("bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
        ];
        for directory in fallbacks {
            for name in names {
                let candidate = directory.join(name);
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

async fn run_agy_usage_cli(
    executable: &Path,
    isolated_home: Option<&Path>,
) -> Result<Vec<u8>, CliRefreshError> {
    let mut command = Command::new(executable);
    command
        .args(["-p", "/usage", "--output-format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "1")
        .env_remove("AGY_ACP_FORCE_FILE_STORAGE")
        .env_remove("GEMINI_HOME")
        .env_remove("ANTIGRAVITY_GEMINI_DIR")
        .env_remove("ANTIGRAVITY_APP_DATA_DIR")
        .env_remove("ANTIGRAVITY_LS_ADDRESS")
        .env_remove("ANTIGRAVITY_CSRF_TOKEN")
        .env_remove("ANTIGRAVITY_AGENT")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("AGY_ADC_AUTH");
    #[cfg(unix)]
    command.env("BROWSER", "/bin/true");
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    if let Some(home) = isolated_home {
        command
            .env("HOME", home)
            .env("AGY_CLI_FORCE_FILE_TOKEN_STORAGE", "1")
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_CLIENT")
            .env_remove("SSH_TTY");
        #[cfg(target_os = "windows")]
        command.env("USERPROFILE", home);
    }

    let mut child = command
        .spawn()
        .map_err(|error| CliRefreshError::Failed(format!("Could not start agy: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliRefreshError::Failed("Could not read agy output.".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliRefreshError::Failed("Could not read agy diagnostics.".into()))?;

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout
            .take(MAX_REPORT_BYTES + 1)
            .read_to_end(&mut buf)
            .await
            .map(|_| buf)
    });

    let stderr_watch = async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("Authentication required") || line.contains("visit the URL to log in")
            {
                return true;
            }
        }
        false
    };

    let run = async {
        tokio::pin!(stderr_watch);
        let auth_prompted = tokio::select! {
            prompted = &mut stderr_watch => prompted,
            status = child.wait() => {
                let _ = status;
                false
            }
        };
        if auth_prompted {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(CliRefreshError::AuthRequired);
        }
        let _ = child.wait().await;
        let output = stdout_task
            .await
            .map_err(|_| CliRefreshError::Failed("Antigravity CLI reader failed.".into()))?
            .map_err(|error| CliRefreshError::Failed(format!("Could not read agy output: {error}")))?;
        if output.len() as u64 > MAX_REPORT_BYTES {
            return Err(CliRefreshError::Failed("The Antigravity report is too large.".into()));
        }
        Ok(output)
    };

    match tokio::time::timeout(CLI_REFRESH_TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(CliRefreshError::Failed(
                "Antigravity CLI timed out; showing the last update".into(),
            ))
        }
    }
}

async fn fetch_t3_plan_tier(executable: &Path, profile: &Path) -> Option<String> {
    let mut command = Command::new(executable);
    command
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
        .env_remove("ANTIGRAVITY_GEMINI_DIR")
        .env_remove("ANTIGRAVITY_APP_DATA_DIR")
        .env_remove("ANTIGRAVITY_LS_ADDRESS")
        .env_remove("ANTIGRAVITY_CSRF_TOKEN")
        .env_remove("ANTIGRAVITY_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let mut lines = BufReader::new(stdout.take(256 * 1024)).lines();

    let tier = tokio::time::timeout(ACP_TIER_TIMEOUT, async {
        let requests = [
            (
                1_u64,
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {"name": "agents-usage", "version": env!("CARGO_PKG_VERSION")}
                }),
            ),
            (2_u64, "authenticate", json!({"methodId": "oauth-personal"})),
        ];
        let mut plan = None;
        for (id, method, params) in requests {
            let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            stdin
                .write_all(format!("{request}\n").as_bytes())
                .await
                .ok()?;
            let _ = stdin.flush().await;
            loop {
                let line = lines.next_line().await.ok()??;
                let response: Value = serde_json::from_str(&line).ok()?;
                if response.get("id").and_then(Value::as_u64) == Some(id) {
                    if response.get("error").is_some() {
                        return None;
                    }
                    if id == 2 {
                        let tier_obj = response.pointer("/result/_meta/antigravity/tier");
                        plan = text(tier_obj.and_then(|value| value.get("name")), 40).or_else(
                            || {
                                (tier_obj
                                    .and_then(|value| value.get("id"))
                                    .and_then(Value::as_str)
                                    == Some("free-tier"))
                                .then(|| "Free".into())
                            },
                        );
                    }
                    break;
                }
            }
        }
        plan
    })
    .await
    .ok()
    .flatten();

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;
    tier
}

/// Refresh one T3 Code Antigravity profile by invoking the official CLI against
/// an isolated temporary HOME linked to that profile's token file. Never reads
/// OAuth access/refresh tokens or calls Google quota endpoints directly.
pub async fn read_t3_account(
    profile: &Path,
    cached_plan: Option<String>,
) -> Result<ProviderReading, ProviderError> {
    let email = local_email(profile).ok_or_else(|| {
        ProviderError::Message(
            "Open Antigravity in T3 Code to sign in with Google, then refresh".into(),
        )
    })?;
    let executable = managed_executable(profile).or_else(locate_agy_executable);
    let cli_error = if let Some(executable) = executable {
        let isolated = IsolatedCliHome::for_profile(profile).map_err(|_| {
            ProviderError::Message("Could not prepare isolated Antigravity CLI environment".into())
        })?;
        let output = run_agy_usage_cli(&executable, Some(isolated.path())).await;
        drop(isolated);
        match output {
            Ok(bytes) => {
                let (mut report, _) = SavedQuota::parse(&bytes, chrono::Utc::now().timestamp())
                    .map_err(|error| ProviderError::Message(error.to_owned()))?;
                report.email = Some(email);
                if report.plan.is_none() {
                    report.plan = cached_plan;
                }
                if report.plan.is_none() {
                    report.plan = fetch_t3_plan_tier(&executable, profile).await;
                }
                return Ok(ProviderReading {
                    snapshot: report.snapshot(),
                    notice: None,
                });
            }
            Err(error) => error,
        }
    } else {
        CliRefreshError::NotFound
    };

    if let Some(mut cached) = read_t3_cache_for_profile(profile) {
        cached.email = Some(email);
        if cached.plan.is_none() {
            cached.plan = cached_plan;
        }
        return Ok(ProviderReading {
            snapshot: cached.snapshot(),
            notice: None,
        });
    }

    Err(ProviderError::Message(match cli_error {
        CliRefreshError::AuthRequired => {
            "Open Antigravity in T3 Code to renew its Google sign-in, then refresh".into()
        }
        CliRefreshError::NotFound => {
            "Antigravity CLI was not found; open Antigravity in T3 Code or install agy".into()
        }
        CliRefreshError::Failed(message) => message,
    }))
}

/// Refresh Antigravity quota using the official `agy` CLI when signed in,
/// falling back to T3 Code's non-secret provider status cache when `agy` is
/// unavailable or not signed in. Never reads OAuth tokens or calls Google APIs.
pub async fn refresh_live(previous: Option<SavedQuota>) -> Option<Result<SavedQuota, String>> {
    let saved = read_report();
    let fallback_meta = previous.as_ref().or(saved.as_ref());

    let cli_error = match locate_agy_executable() {
        Some(executable) => match run_agy_usage_cli(&executable, None).await {
            Ok(bytes) => {
                match SavedQuota::parse(&bytes, chrono::Utc::now().timestamp()) {
                    Ok((parsed, _)) => {
                        let report = parsed.with_fallback_metadata(fallback_meta);
                        if let Err(error) = write_report(&report) {
                            eprintln!("antigravity: {error}");
                        }
                        return Some(Ok(report));
                    }
                    Err(error) => CliRefreshError::Failed(error.to_owned()),
                }
            }
            Err(error) => error,
        },
        None => CliRefreshError::NotFound,
    };

    if let Some(t3_cache) = read_t3_cache() {
        let merged = SavedQuota::pick_fresher(saved.or(previous), Some(t3_cache))?;
        return Some(Ok(merged));
    }

    match cli_error {
        CliRefreshError::NotFound => None,
        CliRefreshError::AuthRequired => Some(Err(
            "Sign in to the Antigravity CLI (agy) to refresh live quota".into(),
        )),
        CliRefreshError::Failed(message) => Some(Err(message)),
    }
}

pub fn report_covered_by_t3(config: &AppConfig, report: &SavedQuota) -> bool {
    let mut t3_emails = HashSet::new();
    for account in &config.accounts {
        if account.provider_id != ID || is_manual_home(&account.home) {
            continue;
        }
        if let Some(email) = account
            .identity_email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
            .or_else(|| local_email(&account.home).map(|email| email.to_ascii_lowercase()))
        {
            t3_emails.insert(email);
        }
    }
    if t3_emails.is_empty() {
        return false;
    }
    match report
        .email
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        Some(email) => t3_emails.contains(&email),
        None => true,
    }
}

pub fn set_quota(config: &mut AppConfig, report: Option<SavedQuota>) {
    let report = report.filter(|value| !report_covered_by_t3(config, value));
    if let Some(value) = &report {
        let home = manual_home();
        let has_preference = config
            .accounts
            .iter()
            .any(|account| account.provider_id == ID && is_manual_home(&account.home));
        let pref = config::preference_for_provider_mut(config, ID, &home);
        if value.email.is_some() {
            pref.identity_email.clone_from(&value.email);
        }
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

    #[test]
    fn t3_cache_parser_reads_antigravity_limits_and_preserves_metadata_when_fresher() {
        let cache = json!({
            "driver": "antigravity",
            "usageLimits": {
                "checkedAt": "2026-11-23T09:32:57.918Z",
                "windows": [
                    {"id": "gemini-5h", "usedPercent": 6.0, "resetsAt": "2026-11-23T14:32:57Z", "windowDurationMins": 300},
                    {"id": "gemini-weekly", "usedPercent": 12.5, "resetsAt": "2026-11-29T12:06:46.000Z", "windowDurationMins": 10080},
                    {"id": "3p-weekly", "usedPercent": 0.0, "windowDurationMins": 10080}
                ]
            }
        });
        let parsed = SavedQuota::parse_t3_cache(cache.to_string().as_bytes()).unwrap();
        let older = SavedQuota {
            windows: vec![QuotaWindow {
                id: "gemini-weekly".into(),
                remaining: 0.99,
                reset_at: None,
            }],
            email: Some("user@example.com".into()),
            plan: Some("Google AI Pro".into()),
            saved_at: parsed.saved_at - 600,
        };
        let merged = SavedQuota::pick_fresher(Some(older), Some(parsed)).unwrap();
        let snap = merged.snapshot();
        assert_eq!(snap.email.as_deref(), Some("user@example.com"));
        assert_eq!(snap.plan_type.as_deref(), Some("Google AI Pro"));
        assert_eq!(snap.windows.len(), 3);
        assert_eq!(snap.windows[0].label.as_deref(), Some("5-hour"));
        assert!((snap.windows[0].used_percent - 6.0).abs() < 0.01);
        assert_eq!(snap.windows[2].label.as_deref(), Some("Weekly"));
        assert!((snap.windows[2].used_percent - 12.5).abs() < 0.01);

        // Non-antigravity T3 cache files must be ignored.
        let codex_cache = json!({"driver": "codex", "usageLimits": {"checkedAt": "2026-11-23T09:32:57Z", "windows": [{"id": "gemini-weekly", "usedPercent": 50.0}]}});
        assert!(SavedQuota::parse_t3_cache(codex_cache.to_string().as_bytes()).is_none());
    }

    #[test]
    fn t3_profile_discovery_and_isolated_cli_home_handle_multiple_accounts_safely() {
        let root = std::env::temp_dir().join(format!("agents-usage-ag-profiles-{}", uuid::Uuid::new_v4()));
        let profile_a = root.join("profile-a");
        let profile_b = root.join("profile-b");
        let api_key_profile = root.join("profile-apikey");
        for (profile, auth_type, email) in [
            (&profile_a, "oauth-personal", "first@example.com"),
            (&profile_b, "oauth-personal", "second@example.com"),
            (&api_key_profile, "gemini-api-key", "ignored@example.com"),
        ] {
            let acp = profile.join(PROFILE_DIR);
            fs::create_dir_all(&acp).unwrap();
            fs::write(acp.join(SETTINGS_FILE), json!({"auth": {"type": auth_type}}).to_string()).unwrap();
            fs::write(
                acp.join(TOKEN_FILE),
                json!({"auth_method": "consumer", "id_token_email": email}).to_string(),
            )
            .unwrap();
        }

        assert_eq!(local_email(&profile_a).as_deref(), Some("first@example.com"));
        assert_eq!(local_email(&profile_b).as_deref(), Some("second@example.com"));
        assert_eq!(local_email(&api_key_profile), None);

        let isolated = IsolatedCliHome::for_profile(&profile_a).unwrap();
        let tmp_path = isolated.path().to_path_buf();
        let linked = tmp_path.join(".gemini").join("antigravity-cli").join(TOKEN_FILE);
        assert!(linked.exists());
        drop(isolated);
        assert!(!tmp_path.exists());
        assert!(profile_a.join(PROFILE_DIR).join(TOKEN_FILE).is_file());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn set_quota_never_overwrites_t3_profiles_and_suppresses_covered_manual_reports() {
        let t3_home = PathBuf::from("/tmp/t3/userdata/providers/antigravity/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut config = AppConfig {
            accounts: vec![crate::config::AccountPreference {
                provider_id: ID.into(),
                home: t3_home.clone(),
                identity_email: Some("user@example.com".into()),
                ..crate::config::AccountPreference::default()
            }],
            ..AppConfig::default()
        };
        let covered = SavedQuota {
            windows: vec![QuotaWindow { id: "gemini-weekly".into(), remaining: 0.5, reset_at: None }],
            email: Some("USER@example.com".into()),
            plan: Some("Google AI Pro".into()),
            saved_at: 100,
        };
        set_quota(&mut config, Some(covered));
        assert!(config.antigravity_report.is_none());
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].home, t3_home);

        let unnamed = SavedQuota {
            windows: vec![QuotaWindow { id: "gemini-weekly".into(), remaining: 0.5, reset_at: None }],
            email: None,
            plan: None,
            saved_at: 100,
        };
        set_quota(&mut config, Some(unnamed));
        assert!(config.antigravity_report.is_none());
        assert_eq!(config.accounts.len(), 1);

        let distinct = SavedQuota {
            windows: vec![QuotaWindow { id: "gemini-weekly".into(), remaining: 0.8, reset_at: None }],
            email: Some("separate@example.com".into()),
            plan: None,
            saved_at: 100,
        };
        set_quota(&mut config, Some(distinct));
        assert!(config.antigravity_report.is_some());
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.accounts[0].home, t3_home);
        assert!(is_manual_home(&config.accounts[1].home));
    }

    #[tokio::test]
    #[ignore = "Requires local T3 Code Antigravity profiles and network access"]
    async fn live_t3_antigravity_profiles_return_quota_and_plan() {
        let home = discovery::user_home().expect("HOME");
        let found = t3_candidates(&home);
        assert!(!found.is_empty(), "No T3 Antigravity profiles found");
        for candidate in found {
            let reading = read_t3_account(&candidate.home, None).await.unwrap();
            assert!(!reading.snapshot.windows.is_empty());
            println!(
                "Antigravity live: email={:?} plan={:?} windows={}",
                reading.snapshot.email,
                reading.snapshot.plan_type,
                reading.snapshot.windows.len()
            );
            for w in &reading.snapshot.windows {
                println!(
                    "  - {:?}: {:.1}% remaining (resets_at={:?})",
                    w.label,
                    100.0 - w.used_percent,
                    w.resets_at
                );
            }
        }
    }
}
