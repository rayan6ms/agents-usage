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
        masked_display_name: mask_account_name(&record.display_name).into(),
        color_name: record.color_name.clone().into(),
        custom_color: record.color_name.starts_with('#'),
        account_color,
        email: record.email().into(),
        masked_email: mask_email(record.email()).into(),
        accent,
        show_accent,
        enabled: record.enabled,
        pin_short,
        expanded: record.expanded,
        name_revealed: record.name_revealed,
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
        long_reset_color: long.map(|window| reset_timer_color(window, now)).unwrap_or_default(),
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
        short_reset_color: short.map(|window| reset_timer_color(window, now)).unwrap_or_default(),
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
    if seconds == 0 { return "0m".into(); }
    let total_minutes = (seconds + 59) / 60;
    let total_hours = (seconds + 3_599) / 3_600;
    if total_hours >= 24 {
        let days = total_hours / 24;
        let hours = total_hours % 24;
        format!("{days}d {hours}h")
    } else if total_minutes >= 60 {
        let hours = total_minutes / 60;
        let minutes = total_minutes % 60;
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{total_minutes}m")
    }
}

fn reset_timer_color(window: &RateWindow, now: i64) -> Color {
    let remaining = window.resets_at.unwrap_or(now).saturating_sub(now).max(0) as f32;
    let (minimum, maximum) = if window.duration_mins.unwrap_or(10_080) <= 300 {
        (30.0 * 60.0, 5.0 * 60.0 * 60.0)
    } else {
        (12.0 * 60.0 * 60.0, 7.0 * 24.0 * 60.0 * 60.0)
    };
    let red_weight = ((remaining - minimum) / (maximum - minimum)).clamp(0.0, 1.0);
    let hue = 120.0 * (1.0 - red_weight);
    Color::from_hsva(hue, 0.78, 0.95, 1.0)
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

fn mask_account_name(name: &str) -> String {
    let mut characters = name.chars();
    let Some(first) = characters.next() else { return String::new(); };
    let hidden = characters.count().clamp(3, 10);
    format!("{first}{}", "•".repeat(hidden))
}

pub fn color_from_name(name: &str) -> Color {
    if let Some(color) = parse_hex_color(name) { return color; }
    match name {
        "red" => Color::from_rgb_u8(239, 68, 68),
        "orange" => Color::from_rgb_u8(249, 115, 22),
        "green" => Color::from_rgb_u8(34, 197, 94),
        "cyan" => Color::from_rgb_u8(39, 191, 206),
        "black" => Color::from_rgb_u8(20, 20, 20),
        "yellow" => Color::from_rgb_u8(234, 179, 8),
        "blue" => Color::from_rgb_u8(59, 130, 246),
        "pink" => Color::from_rgb_u8(236, 72, 153),
        "gray" => Color::from_rgb_u8(156, 163, 175),
        "purple" => Color::from_rgb_u8(140, 109, 216),
        _ => Color::from_rgb_u8(39, 191, 206),
    }
}

fn parse_hex_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) { return None; }
    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::from_rgb_u8(red, green, blue))
}

pub fn is_account_color(name: &str) -> bool {
    ACCOUNT_COLORS.contains(&name) || parse_hex_color(name).is_some()
}

pub const ACCOUNT_COLORS: [&str; 10] = [
    "red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink", "gray", "black",
];

#[cfg(test)]
mod tests {
    use super::{ACCOUNT_COLORS, color_from_name, format_countdown, is_account_color, mask_account_name, reset_timer_color};
    use crate::domain::RateWindow;

    #[test]
    fn account_color_validation_matches_the_exposed_palette() {
        for color in ACCOUNT_COLORS {
            assert!(is_account_color(color));
        }
        assert!(!is_account_color(""));
        assert!(!is_account_color("chartreuse"));
        assert!(!ACCOUNT_COLORS.contains(&"white"));
        assert!(is_account_color("#12aBcF"));
        assert!(!is_account_color("#12345"));
        let custom = color_from_name("#12abcf");
        assert_eq!((custom.red(), custom.green(), custom.blue()), (0x12, 0xab, 0xcf));
    }

    #[test]
    fn account_name_mask_keeps_only_a_hint() {
        assert_eq!(mask_account_name("Work"), "W•••");
        assert_eq!(mask_account_name(""), "");
    }

    #[test]
    fn reset_timer_moves_from_red_toward_green() {
        let weekly_far = RateWindow {
            used_percent: 0.0,
            duration_mins: Some(10_080),
            resets_at: Some(7 * 24 * 60 * 60),
        };
        let weekly_near = RateWindow { resets_at: Some(12 * 60 * 60), ..weekly_far.clone() };
        let short_far = RateWindow {
            used_percent: 0.0,
            duration_mins: Some(300),
            resets_at: Some(5 * 60 * 60),
        };
        let short_near = RateWindow { resets_at: Some(30 * 60), ..short_far.clone() };

        let far = reset_timer_color(&weekly_far, 0);
        let near = reset_timer_color(&weekly_near, 0);
        assert!(far.red() > far.green());
        assert!(near.green() > near.red());
        let far = reset_timer_color(&short_far, 0);
        let near = reset_timer_color(&short_near, 0);
        assert!(far.red() > far.green());
        assert!(near.green() > near.red());
    }

    #[test]
    fn countdown_rounds_up_without_flipping_at_unit_boundaries() {
        assert_eq!(format_countdown(7 * 24 * 60 * 60 - 1, 0), "7d 0h");
        assert_eq!(format_countdown(6 * 24 * 60 * 60 + 23 * 60 * 60, 0), "6d 23h");
        assert_eq!(format_countdown(5 * 60 * 60 - 1, 0), "5h 00m");
        assert_eq!(format_countdown(1, 0), "1m");
        assert_eq!(format_countdown(0, 0), "0m");
    }
}
