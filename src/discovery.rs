use crate::config::AppConfig;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn candidate_codex_homes(config: &AppConfig) -> Vec<PathBuf> {
    let mut raw = Vec::new();

    if let Some(value) = std::env::var_os("CODEX_HOME") {
        raw.push(PathBuf::from(value));
    }
    if let Some(value) = std::env::var_os("AGENTS_USAGE_CODEX_HOMES") {
        raw.extend(std::env::split_paths(&value));
    }

    raw.extend(config.accounts.iter().map(|account| account.home.clone()));
    raw.extend(config.additional_codex_homes.iter().cloned());

    if let Some(home) = user_home() {
        // Deliberately shallow: inspect direct children only and select them by
        // a Codex-owned marker rather than by the user's directory naming scheme.
        let mut marked = Vec::new();
        if let Ok(entries) = fs::read_dir(&home) {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_marked_codex_home(&path) {
                    marked.push(path);
                }
            }
        }
        marked.sort();
        raw.extend(marked);
    }

    dedupe_existing_dirs(raw)
}

fn is_marked_codex_home(path: &Path) -> bool {
    path.is_dir() && path.join("auth.json").is_file()
}

pub fn user_home() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn dedupe_existing_dirs(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if !path.is_dir() {
            continue;
        }
        let canonical = canonicalish(&path);
        if seen.insert(canonical.clone()) {
            out.push(path);
        }
    }
    out
}

fn canonicalish(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::is_marked_codex_home;
    use std::fs;

    #[test]
    fn auto_discovery_uses_a_codex_marker_not_a_directory_name() {
        let root = std::env::temp_dir().join(format!("agents-usage-discovery-{}", uuid::Uuid::new_v4()));
        let arbitrary = root.join("work-account-any-name");
        let misleading = root.join(".codex_p42");
        fs::create_dir_all(&arbitrary).unwrap();
        fs::create_dir_all(&misleading).unwrap();
        fs::write(arbitrary.join("auth.json"), b"{}").unwrap();

        assert!(is_marked_codex_home(&arbitrary));
        assert!(!is_marked_codex_home(&misleading));

        fs::remove_dir_all(root).unwrap();
    }
}
