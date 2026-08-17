use crate::domain::{AccountRecord, RateWindow};
use crate::{AccountView, ResetCreditView};
use chrono::{Datelike, Local, TimeZone};
use slint::{Color, ModelRc, VecModel};
use std::rc::Rc;

pub const PANEL_HEADER_HEIGHT: f32 = 41.0;
pub const PANEL_BOTTOM_PADDING: f32 = 17.0;
pub const EMPTY_CONTENT_HEIGHT: f32 = 72.0;
pub const PANEL_MAX_HEIGHT: f32 = 680.0;

pub fn account_view(record: &AccountRecord, enabled_count: usize, pin_short_global: bool) -> AccountView {
    let snapshot = record.snapshot.as_ref();
    let mut windows: Vec<&RateWindow> = snapshot
        .map(|snapshot| snapshot.windows.iter().collect())
        .unwrap_or_default();
    windows.sort_by_key(|window| window.duration_mins.unwrap_or(u64::MAX));

    let short = if windows.len() >= 2 { windows.first().copied() } else { None };
    let long = windows.last().copied();
    let now = chrono::Utc::now().timestamp();

    let provider_color = Color::from_rgb_u8(255, 255, 255);
    let account_color = color_from_name(&record.color_name);
    let accent = if enabled_count <= 1 { provider_color } else { account_color };
    let show_accent = enabled_count > 1;

    let pin_short = (pin_short_global || record.pin_short) && short.is_some();
    let reset_count = snapshot.map(|value| value.reset_available_count).unwrap_or(0);
    let mut credits = Vec::new();
    if let Some(snapshot) = snapshot {
        for credit in snapshot.reset_credits.iter().filter(|credit| {
            credit.status.as_deref().map(|status| status == "available").unwrap_or(true)
        }) {
            credits.push(ResetCreditView {
                id: credit.id.clone().unwrap_or_default().into(),
                title: credit.title.clone().unwrap_or_else(|| "Rate-limit reset".into()).into(),
                expiry: credit
                    .expires_at
                    .map(|value| format_expiry(value, now))
                    .unwrap_or_else(|| credit.description.clone().unwrap_or_default())
                    .into(),
            });
        }
        if reset_count > credits.len() as u32 {
            credits.push(ResetCreditView {
                id: "__next__".into(),
                title: "Rate-limit reset".into(),
                expiry: "Next available reset".into(),
            });
        }
    }

    let has_long_reset = long.and_then(|window| window.resets_at).is_some();
    let has_short_reset = short.and_then(|window| window.resets_at).is_some();
    let has_hidden_details = (!pin_short && short.is_some())
        || has_long_reset
        || has_short_reset
        || reset_count > 0;

    let detail_height = if record.expanded {
        detail_target_height(!pin_short && short.is_some(), reset_count > 0, credits.len())
    } else {
        0.0
    };
    let target_detail_height = detail_target_height(!pin_short && short.is_some(), reset_count > 0, credits.len());
    let row_height = 53.0
        + if pin_short { 20.0 } else { 0.0 }
        + if record.last_error.is_some() && long.is_some() { 18.0 } else { 0.0 }
        + detail_height;

    let long_label = long
        .map(|window| window_label(window.duration_mins, snapshot.and_then(|s| s.bucket_name.as_deref()), true))
        .unwrap_or_else(|| "Usage".into());
    let short_label = short
        .map(|window| window_label(window.duration_mins, snapshot.and_then(|s| s.bucket_name.as_deref()), false))
        .unwrap_or_else(|| "5-hour".into());

    AccountView {
        id: record.id.clone().into(),
        provider_id: record.provider_id.clone().into(),
        display_name: record.display_name.clone().into(),
        color_name: record.color_name.clone().into(),
        email: record.email().into(),
        masked_email: mask_email(record.email()).into(),
        accent,
        show_accent,
        enabled: record.enabled,
        pin_short,
        expanded: record.expanded,
        email_revealed: record.email_revealed,
        confirm_credit_id: record.confirm_credit_id.clone().into(),
        has_usage: long.is_some(),
        has_error: record.last_error.is_some(),
        error_text: record.last_error.clone().unwrap_or_default().into(),
        long_label: long_label.into(),
        long_remaining: long.map(remaining_fraction).unwrap_or(0.0),
        long_percent_text: long.map(percent_text).unwrap_or_default().into(),
        has_long_reset_time: has_long_reset,
        long_reset_text: long
            .and_then(|window| window.resets_at)
            .map(|value| format_countdown(value, now))
            .unwrap_or_default()
            .into(),
        has_short_limit: short.is_some(),
        short_label: short_label.into(),
        short_remaining: short.map(remaining_fraction).unwrap_or(0.0),
        short_percent_text: short.map(percent_text).unwrap_or_default().into(),
        has_short_reset_time: has_short_reset,
        short_reset_text: short
            .and_then(|window| window.resets_at)
            .map(|value| format_countdown(value, now))
            .unwrap_or_default()
            .into(),
        has_reset_credits: reset_count > 0,
        reset_count_text: if reset_count == 1 { "1 available".into() } else { format!("{reset_count} available").into() },
        reset_credits: Rc::new(VecModel::from(credits)).into(),
        has_hidden_details,
        detail_height_px: target_detail_height,
        row_height_px: row_height,
    }
}

pub fn panel_height(records: &[AccountRecord], pin_short_global: bool) -> f32 {
    let enabled_count = records.iter().filter(|record| record.enabled).count();
    if enabled_count == 0 {
        return PANEL_HEADER_HEIGHT + EMPTY_CONTENT_HEIGHT;
    }
    let rows = records
        .iter()
        .filter(|record| record.enabled)
        .map(|record| account_view(record, enabled_count, pin_short_global).row_height_px)
        .sum::<f32>();
    (PANEL_HEADER_HEIGHT + rows + PANEL_BOTTOM_PADDING).clamp(128.0, PANEL_MAX_HEIGHT)
}

pub fn model(records: &[AccountRecord], pin_short_global: bool) -> (ModelRc<AccountView>, usize) {
    let enabled_count = records.iter().filter(|record| record.enabled).count();
    let rows = records
        .iter()
        .map(|record| account_view(record, enabled_count, pin_short_global))
        .collect::<Vec<_>>();
    (Rc::new(VecModel::from(rows)).into(), enabled_count)
}

fn detail_target_height(has_hidden_short: bool, has_reset_count: bool, reset_rows: usize) -> f32 {
    (if has_hidden_short { 20.0 } else { 0.0 })
        + (if has_reset_count { 18.0 } else { 0.0 })
        + (if reset_rows > 0 { 3.0 } else { 0.0 })
        + reset_rows as f32 * 30.0
        + (if reset_rows > 0 { 9.0 } else if has_reset_count { 5.0 } else { 0.0 })
}

fn remaining_fraction(window: &RateWindow) -> f32 {
    ((100.0 - window.used_percent).clamp(0.0, 100.0)) / 100.0
}

fn percent_text(window: &RateWindow) -> String {
    format!("{:.0}%", remaining_fraction(window) * 100.0)
}

fn window_label(duration_mins: Option<u64>, fallback: Option<&str>, long: bool) -> String {
    match duration_mins {
        Some(10_080) => "Weekly".into(),
        Some(300) => "5-hour".into(),
        Some(minutes) if minutes % 1_440 == 0 => {
            let days = minutes / 1_440;
            if days == 7 { "Weekly".into() } else { format!("{days}-day") }
        }
        Some(minutes) if minutes % 60 == 0 => format!("{}-hour", minutes / 60),
        Some(minutes) => format!("{minutes}-min"),
        None => fallback.map(str::to_string).unwrap_or_else(|| if long { "Usage".into() } else { "Short".into() }),
    }
}

fn format_countdown(timestamp: i64, now: i64) -> String {
    let seconds = (timestamp - now).max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn format_expiry(timestamp: i64, now: i64) -> String {
    let date = Local.timestamp_opt(timestamp, 0).single();
    let date_text = date
        .map(|value| {
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{} {}", MONTHS[value.month0() as usize], value.day())
        })
        .unwrap_or_else(|| "Unknown expiry".into());
    let days = ((timestamp - now).max(0) + 86_399) / 86_400;
    if days > 0 { format!("Expires {date_text} · {days} days") } else { format!("Expires {date_text}") }
}

fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        if email.is_empty() { return String::new(); }
        let first = email.chars().next().unwrap_or('•');
        return format!("{first}••••");
    };
    let first = local.chars().next().unwrap_or('•');
    let count = local.chars().count().saturating_sub(1).clamp(3, 8);
    format!("{first}{}@{domain}", "•".repeat(count))
}

pub fn color_from_name(name: &str) -> Color {
    match name {
        "red" => Color::from_rgb_u8(239, 68, 68),
        "orange" => Color::from_rgb_u8(249, 115, 22),
        "green" => Color::from_rgb_u8(34, 197, 94),
        "cyan" => Color::from_rgb_u8(39, 191, 206),
        "black" => Color::from_rgb_u8(20, 20, 20),
        "yellow" => Color::from_rgb_u8(234, 179, 8),
        "blue" => Color::from_rgb_u8(59, 130, 246),
        "pink" => Color::from_rgb_u8(236, 72, 153),
        "white" => Color::from_rgb_u8(245, 245, 245),
        "gray" => Color::from_rgb_u8(156, 163, 175),
        "purple" => Color::from_rgb_u8(140, 109, 216),
        _ => Color::from_rgb_u8(39, 191, 206),
    }
}

pub fn is_account_color(name: &str) -> bool {
    ACCOUNT_COLORS.contains(&name)
}

pub const ACCOUNT_COLORS: [&str; 11] = [
    "cyan", "purple", "blue", "green", "orange", "pink", "yellow", "red", "gray", "white", "black",
];

#[cfg(test)]
mod tests {
    use super::{ACCOUNT_COLORS, is_account_color};

    #[test]
    fn account_color_validation_matches_the_exposed_palette() {
        for color in ACCOUNT_COLORS {
            assert!(is_account_color(color));
        }
        assert!(!is_account_color(""));
        assert!(!is_account_color("chartreuse"));
    }
}
