use crate::domain::PendingReset;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub blur_emails: bool,
    #[serde(default)]
    pub pin_short_global: bool,
    #[serde(default)]
    pub codex_executable: Option<PathBuf>,
    #[serde(default)]
    pub additional_codex_homes: Vec<PathBuf>,
    #[serde(default)]
    pub accounts: Vec<AccountPreference>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: schema_version(),
            blur_emails: false,
            pin_short_global: false,
            codex_executable: None,
            additional_codex_homes: Vec::new(),
            accounts: Vec::new(),
        }
    }
}

fn schema_version() -> u32 { 1 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountPreference {
    pub home: PathBuf,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub pin_short: bool,
    #[serde(default)]
    pub expanded: bool,
}

fn default_true() -> bool { true }

impl Default for AccountPreference {
    fn default() -> Self {
        Self {
            home: PathBuf::new(),
            display_name: None,
            color: None,
            enabled: true,
            pin_short: false,
            expanded: false,
        }
    }
}

pub fn app_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(value) = std::env::var_os("APPDATA") {
            return PathBuf::from(value).join("Agents Usage");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Agents Usage");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(value) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(value).join("agents-usage");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join("agents-usage");
        }
    }
    PathBuf::from(".").join(".agents-usage")
}

pub fn config_path() -> PathBuf { app_config_dir().join("config.toml") }
pub fn pending_reset_path() -> PathBuf { app_config_dir().join("pending-reset.json") }

pub fn load() -> AppConfig {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
            eprintln!("settings: could not parse {}: {error}", path.display());
            preserve_invalid_file(&path, "config");
            AppConfig::default()
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => AppConfig::default(),
        Err(error) => {
            eprintln!("settings: could not read {}: {error}", path.display());
            AppConfig::default()
        }
    }
}

pub fn save(config: &AppConfig) -> io::Result<()> {
    let path = config_path();
    let text = toml::to_string_pretty(config)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    atomic_write(&path, text.as_bytes())
}


pub fn preference_for_mut<'a>(config: &'a mut AppConfig, home: &Path) -> &'a mut AccountPreference {
    if let Some(index) = config.accounts.iter().position(|pref| same_path(&pref.home, home)) {
        return &mut config.accounts[index];
    }
    config.accounts.push(AccountPreference {
        home: home.to_path_buf(),
        ..AccountPreference::default()
    });
    config.accounts.last_mut().expect("just pushed account preference")
}

fn same_path(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

pub fn load_pending_reset() -> io::Result<Option<PendingReset>> {
    let path = pending_reset_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| io::Error::new(
            io::ErrorKind::InvalidData,
            format!("could not parse {}: {error}", path.display()),
        ))
}

pub fn save_pending_reset(pending: &PendingReset) -> io::Result<()> {
    let text = serde_json::to_vec_pretty(pending)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    atomic_write(&pending_reset_path(), &text)
}

pub fn clear_pending_reset() -> io::Result<()> {
    match fs::remove_file(pending_reset_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    match replace_file(&tmp, path) {
        Ok(()) => {
            #[cfg(unix)]
            if let Some(parent) = path.parent() {
                fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let destination = destination.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: both paths are stable, NUL-terminated UTF-16 buffers for the
    // duration of the call. MoveFileExW provides replace-existing semantics
    // that std::fs::rename intentionally does not provide on Windows.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn preserve_invalid_file(path: &Path, label: &str) {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let backup = path.with_extension(format!("invalid-{suffix}"));
    match fs::rename(path, &backup) {
        Ok(()) => eprintln!(
            "settings: preserved invalid {label} as {}",
            backup.display()
        ),
        Err(error) => eprintln!(
            "settings: could not preserve invalid {label} {}: {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{AccountPreference, atomic_write};
    use std::fs;

    #[test]
    fn atomic_write_replaces_content_without_leaving_temp_files() {
        let root = std::env::temp_dir().join(format!("agents-usage-config-{}", uuid::Uuid::new_v4()));
        let path = root.join("config.toml");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn older_account_settings_default_to_collapsed() {
        let preference: AccountPreference = toml::from_str(
            r#"
home = "/tmp/example"
enabled = true
pin_short = false
"#,
        )
        .unwrap();

        assert!(!preference.expanded);
    }

    #[test]
    fn expanded_account_state_round_trips() {
        let preference = AccountPreference {
            expanded: true,
            ..AccountPreference::default()
        };
        let serialized = toml::to_string(&preference).unwrap();
        let restored: AccountPreference = toml::from_str(&serialized).unwrap();

        assert!(restored.expanded);
    }
}
