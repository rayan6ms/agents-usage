use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct RateWindow {
    pub used_percent: f32,
    pub duration_mins: Option<u64>,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ResetCredit {
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub expires_at: Option<i64>,
    pub status: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UsageSnapshot {
    pub email: Option<String>,
    pub bucket_name: Option<String>,
    pub windows: Vec<RateWindow>,
    pub reset_available_count: u32,
    pub reset_credits: Vec<ResetCredit>,
}

#[derive(Clone, Debug)]
pub struct AccountRecord {
    pub id: String,
    pub home: PathBuf,
    pub provider_id: String,
    pub display_name: String,
    pub color_name: String,
    pub enabled: bool,
    pub pin_short: bool,
    pub expanded: bool,
    pub email_revealed: bool,
    pub confirm_credit_id: String,
    pub snapshot: Option<UsageSnapshot>,
    pub last_error: Option<String>,
}

impl AccountRecord {
    pub fn email(&self) -> &str {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.email.as_deref())
            .unwrap_or("")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingReset {
    pub account_id: String,
    pub codex_home: PathBuf,
    pub credit_id: Option<String>,
    pub idempotency_key: String,
    pub started_at_unix: i64,
}
