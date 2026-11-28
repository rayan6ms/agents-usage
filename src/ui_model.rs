use crate::config::AppConfig;
use crate::domain::{AccountRecord, RateWindow};
use crate::{AccountView, LimitView, ResetCreditView};
use chrono::{Datelike, Local, TimeZone, Timelike};
use slint::{Color, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::collections::HashMap;
use std::rc::Rc;

// The 40 px header, 1 px divider, and 2 px outer frame.
pub const PANEL_HEADER_HEIGHT: f32 = 43.0;
pub const PANEL_BOTTOM_PADDING: f32 = 6.0;
pub const EMPTY_CONTENT_HEIGHT: f32 = 72.0;
pub const PANEL_MAX_HEIGHT: f32 = 680.0;

pub fn account_view(
    record: &AccountRecord,
    enabled_count: usize,
    config: &AppConfig,
    show_separator: bool,
) -> AccountView {
    let show_banked_resets = config.show_banked_resets;
    let show_antigravity_third_party_limits = config.show_antigravity_third_party_limits;
    let snapshot = record.snapshot.as_ref();
    let plan_name = plan_display_name(snapshot.and_then(|value| value.plan_type.as_deref()));
    let mut windows: Vec<&RateWindow> = snapshot
        .map(|snapshot| {
            snapshot
                .windows
                .iter()
                .filter(|window| {
                    show_antigravity_third_party_limits
                        || record.provider_id != crate::antigravity::ID
                        || !crate::antigravity::is_third_party_window(window)
                })
                .collect()
        })
        .unwrap_or_default();
    windows.sort_by_key(|window| window.duration_mins.unwrap_or(u64::MAX));
    if let Some(index) = primary_window_index(&windows) {
        let primary = windows.remove(index);
        windows.push(primary);
    }

    let short = if windows.len() >= 2 { windows.first().copied() } else { None };
    let long = windows.last().copied();
    let now = chrono::Utc::now().timestamp();

    let account_color = color_from_name(&record.color_name);
    let usage_color = if enabled_count <= 1 {
        Color::from_rgb_u8(255, 255, 255)
    } else {
        visible_usage_color(account_color)
    };

    // The account chevron is the single control for revealing the short
    // (5-hour) window. Keep the legacy preference field readable for config
    // compatibility, but derive the rendered state from the row expansion.
    let show_short = record.expanded && short.is_some();
    let detail_windows = windows
        .iter()
        .copied()
        .filter(|window| {
            !(long.is_some_and(|main| std::ptr::eq(*window, main))
                || show_short && short.is_some_and(|pinned| std::ptr::eq(*window, pinned)))
        })
        .collect::<Vec<_>>();
    let mut reset_count = 0;
    let mut credits = Vec::new();
    if show_banked_resets && let Some(snapshot) = snapshot {
        reset_count = snapshot.reset_available_count;
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
        reset_count = reset_count.max(credits.len() as u32);
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
    let has_hidden_details = short.is_some()
        || !detail_windows.is_empty()
        || has_long_reset
        || has_short_reset
        || reset_count > 0;

    let detail_height = if record.expanded {
        detail_target_height(detail_windows.len(), reset_count > 0, credits.len())
    } else {
        0.0
    };
    let target_detail_height = detail_target_height(detail_windows.len(), reset_count > 0, credits.len());
    // Header (24) + primary limit line (20) + vertical padding (10) + gap (2).
    let row_height = 56.0
        + if show_short { 20.0 } else { 0.0 }
        + if record.last_error.is_some() && long.is_some() { 18.0 } else { 0.0 }
        + detail_height;

    let long_label = long
        .map(|window| window_label(window, snapshot.and_then(|s| s.bucket_name.as_deref()), true))
        .unwrap_or_else(|| "Usage".into());
    let short_label = short
        .map(|window| window_label(window, snapshot.and_then(|s| s.bucket_name.as_deref()), false))
        .unwrap_or_else(|| "5-hour".into());

    AccountView {
        id: record.id.clone().into(),
        provider_id: record.provider_id.clone().into(),
        display_name: record.display_name.clone().into(),
        masked_display_name: mask_account_name(&record.display_name).into(),
        color_name: record.color_name.clone().into(),
        custom_color: record.color_name.starts_with('#'),
        account_color,
        usage_color,
        email: record.email().into(),
        masked_email: mask_email(record.email()).into(),
        has_plan: !plan_name.is_empty(),
        plan_name: plan_name.into(),
        show_accent: false,
        enabled: record.enabled,
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
            .map(|value| format_reset_text(value, now, config))
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
            .map(|value| format_reset_text(value, now, config))
            .unwrap_or_default()
            .into(),
        short_reset_color: short.map(|window| reset_timer_color(window, now)).unwrap_or_default(),
        detail_limits: Rc::new(VecModel::from(
            detail_windows
                .into_iter()
                .map(|window| LimitView {
                    label: window_label(window, snapshot.and_then(|s| s.bucket_name.as_deref()), false).into(),
                    remaining: remaining_fraction(window),
                    percent_text: percent_text(window).into(),
                    has_reset_time: window.resets_at.is_some(),
                    reset_text: window.resets_at.map(|value| format_reset_text(value, now, config)).unwrap_or_default().into(),
                    reset_color: reset_timer_color(window, now),
                })
                .collect::<Vec<_>>(),
        )).into(),
        has_reset_credits: reset_count > 0,
        reset_count_text: if reset_count == 1 { "1 available".into() } else { format!("{reset_count} available").into() },
        reset_credits: Rc::new(VecModel::from(credits)).into(),
        has_hidden_details,
        show_separator,
        detail_height_px: target_detail_height,
        row_height_px: row_height,
    }
}

pub fn plan_display_name(plan_type: Option<&str>) -> String {
    let Some(plan_type) = plan_type.map(str::trim).filter(|value| !value.is_empty()) else {
        return String::new();
    };
    match plan_type.to_ascii_lowercase().as_str() {
        "free" => "Free".into(),
        "go" => "Go".into(),
        "plus" => "Plus".into(),
        "pro" => "Pro".into(),
        "google ai pro" => "Google AI Pro".into(),
        "google ai ultra" => "Google AI Ultra".into(),
        "google ai plus" => "Google AI Plus".into(),
        "team" => "Team".into(),
        "business" => "Business".into(),
        "enterprise" => "Enterprise".into(),
        "edu" | "education" => "Edu".into(),
        other => other
            .split(['-', '_', ' '])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut characters = part.chars();
                characters
                    .next()
                    .map(|first| first.to_uppercase().chain(characters).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

pub fn settings_panel_height(dashboard_height: f32) -> f32 {
    // Settings needs a usable scroll viewport even with zero or one account.
    // Keep taller dashboards unchanged, without enlarging the dashboard itself.
    dashboard_height.max(520.0)
}

pub fn panel_height(
    records: &[AccountRecord],
    config: &AppConfig,
) -> f32 {
    let enabled_count = records.iter().filter(|record| record.enabled).count();
    if enabled_count == 0 {
        return PANEL_HEADER_HEIGHT + EMPTY_CONTENT_HEIGHT;
    }
    let rows = records
        .iter()
        .filter(|record| record.enabled)
        .map(|record| {
            account_view(
                record,
                enabled_count,
                config,
                false,
            )
            .row_height_px
        })
        .sum::<f32>();
    (PANEL_HEADER_HEIGHT + rows + PANEL_BOTTOM_PADDING).min(PANEL_MAX_HEIGHT)
}

pub fn model(
    records: &[AccountRecord],
    config: &AppConfig,
) -> (ModelRc<AccountView>, usize) {
    let mut enabled_provider_counts = HashMap::<&str, usize>::new();
    for record in records.iter().filter(|record| record.enabled) {
        *enabled_provider_counts.entry(record.provider_id.as_str()).or_default() += 1;
    }
    let enabled_count = enabled_provider_counts.values().sum();
    let last_enabled_index = records.iter().rposition(|record| record.enabled);
    let rows = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let show_separator = record.enabled && Some(index) != last_enabled_index;
            let mut row = account_view(
                record,
                enabled_count,
                config,
                show_separator,
            );
            row.show_accent = record.enabled && enabled_provider_counts
                .get(record.provider_id.as_str()).is_some_and(|&count| count > 1);
            row
        })
        .collect::<Vec<_>>();
    (Rc::new(VecModel::from(rows)).into(), enabled_count)
}

fn detail_target_height(limit_rows: usize, has_reset_count: bool, reset_rows: usize) -> f32 {
    limit_rows as f32 * 20.0
        + (if has_reset_count { 18.0 } else { 0.0 })
        + (if reset_rows > 0 { 3.0 } else { 0.0 })
        + reset_rows as f32 * 30.0
        + (if reset_rows > 0 { 9.0 } else if has_reset_count { 5.0 } else { 0.0 })
}

fn remaining_fraction(window: &RateWindow) -> f32 {
    ((100.0 - window.used_percent).clamp(0.0, 100.0)) / 100.0
}

fn primary_window_index(windows: &[&RateWindow]) -> Option<usize> {
    let canonical = windows
        .iter()
        .enumerate()
        .filter_map(|(index, window)| {
            let label = window.label.as_deref().unwrap_or_default();
            let priority = match label {
                "Monthly" => 4,
                "Weekly" => 3,
                _ => return None,
            };
            Some((index, priority, window.duration_mins.unwrap_or_default()))
        })
        .max_by_key(|(_, priority, duration)| (*priority, *duration))
        .map(|(index, _, _)| index);
    canonical.or_else(|| {
        windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.duration_mins.is_some())
            .max_by_key(|(_, window)| window.duration_mins)
            .map(|(index, _)| index)
    }).or_else(|| {
        windows
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.used_percent.total_cmp(&right.used_percent))
            .map(|(index, _)| index)
    })
}

fn percent_text(window: &RateWindow) -> String {
    format!("{:.0}%", remaining_fraction(window) * 100.0)
}

fn window_label(window: &RateWindow, fallback: Option<&str>, long: bool) -> String {
    if let Some(label) = window.label.as_deref().filter(|label| !label.trim().is_empty()) {
        return label.to_string();
    }
    match window.duration_mins {
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
    let seconds = timestamp.saturating_sub(now).max(0);
    if seconds == 0 { return "0m".into(); }
    let total_minutes = seconds.saturating_add(59) / 60;
    let total_hours = seconds.saturating_add(3_599) / 3_600;
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

fn format_reset_text(timestamp: i64, now: i64, config: &AppConfig) -> String {
    if config.show_reset_clock_time {
        format_reset_clock(timestamp, now, config.reset_clock_24_hour, &Local)
    } else {
        format_countdown(timestamp, now)
    }
}

fn format_reset_clock<T: TimeZone>(timestamp: i64, now: i64, use_24_hour: bool, zone: &T) -> String {
    // Use the space formerly occupied by "resets in" and compact redundant
    // countdown spacing/zero units so the local clock fits cleanly.
    let countdown = format_countdown(timestamp, now)
        .replace(' ', "")
        .replace("h00m", "h")
        .replace("d0h", "d");
    if timestamp <= now { return countdown; }
    let Some(date) = zone.timestamp_opt(timestamp, 0).single() else { return countdown; };
    let clock = if use_24_hour {
        format!("{:02}:{:02}", date.hour(), date.minute())
    } else {
        let hour = match date.hour() % 12 { 0 => 12, value => value };
        format!("{hour}:{:02}{}", date.minute(), if date.hour() < 12 { "am" } else { "pm" })
    };
    format!("{countdown} ({clock})")
}

fn reset_timer_color(window: &RateWindow, now: i64) -> Color {
    let remaining = window.resets_at.unwrap_or(now).saturating_sub(now).max(0) as f32;
    let maximum = window.duration_mins.unwrap_or(10_080) as f32 * 60.0;
    let minimum = maximum / 10.0;
    let red_weight = ((remaining - minimum) / (maximum - minimum)).clamp(0.0, 1.0);
    let hue = 120.0 * (1.0 - red_weight);
    Color::from_hsva(hue, 0.78, 0.95, 1.0)
}

fn format_expiry(timestamp: i64, now: i64) -> String {
    let date = Local.timestamp_opt(timestamp, 0).single();
    let date_text = date
        .map(|value| {
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{} {} at {}", MONTHS[value.month0() as usize], value.day(), value.format("%H:%M"))
        })
        .unwrap_or_else(|| "Unknown expiry".into());
    let days = ((timestamp - now).max(0) + 86_399) / 86_400;
    match days {
        0 => format!("Expires {date_text}"),
        1 => format!("Expires {date_text} · 1 day"),
        _ => format!("Expires {date_text} · {days} days"),
    }
}

pub(crate) fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        if email.is_empty() { return String::new(); }
        let first = email.chars().next().unwrap_or('•');
        return format!("{first}••••");
    };
    let first = local.chars().next().unwrap_or('•');
    let count = local.chars().count().saturating_sub(1).clamp(3, 8);
    format!("{first}{}@{domain}", "•".repeat(count))
}

pub(crate) fn mask_account_name(name: &str) -> String {
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

pub fn visible_usage_color(color: Color) -> Color {
    const TARGET_LUMINANCE: f32 = 0.24;
    if relative_luminance(color) >= TARGET_LUMINANCE {
        return color;
    }

    let mut lower = 0.0_f32;
    let mut upper = 1.0_f32;
    for _ in 0..10 {
        let blend = (lower + upper) / 2.0;
        let candidate = blend_toward_white(color, blend);
        if relative_luminance(candidate) < TARGET_LUMINANCE {
            lower = blend;
        } else {
            upper = blend;
        }
    }
    blend_toward_white(color, upper)
}

fn blend_toward_white(color: Color, amount: f32) -> Color {
    let blend = |channel: u8| {
        (channel as f32 + (255.0 - channel as f32) * amount).round() as u8
    };
    Color::from_rgb_u8(blend(color.red()), blend(color.green()), blend(color.blue()))
}

fn relative_luminance(color: Color) -> f32 {
    let linear = |channel: u8| {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.red())
        + 0.7152 * linear(color.green())
        + 0.0722 * linear(color.blue())
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

/// Downsample an icon in linear-light premultiplied RGBA to an exact square
/// physical pixel size so Slint's nearest-neighbor software texture blitter
/// renders it 1:1 with smooth anti-aliased edges.
pub fn resample_icon_square(source: &Image, target_px: u32) -> Image {
    let target_px = target_px.max(1);
    let Some(src) = source.to_rgba8_premultiplied() else {
        return source.clone();
    };
    let src_w = src.width();
    let src_h = src.height();
    if src_w == 0 || src_h == 0 || (src_w == target_px && src_h == target_px) {
        return source.clone();
    }

    let pixels = src.as_slice();
    let mut linear = Vec::with_capacity(pixels.len());
    for pixel in pixels {
        if pixel.a == 0 {
            linear.push([0.0_f32; 4]);
        } else {
            let a = pixel.a as f32 / 255.0;
            let inv_a = 1.0 / (pixel.a as f32);
            let r = srgb_to_linear((pixel.r as f32 * inv_a).clamp(0.0, 1.0)) * a;
            let g = srgb_to_linear((pixel.g as f32 * inv_a).clamp(0.0, 1.0)) * a;
            let b = srgb_to_linear((pixel.b as f32 * inv_a).clamp(0.0, 1.0)) * a;
            linear.push([r, g, b, a]);
        }
    }

    let dst_f = target_px as f32;
    let src_wf = src_w as f32;
    let src_hf = src_h as f32;
    let scale = (dst_f / src_wf).min(dst_f / src_hf);
    let off_x = (dst_f - src_wf * scale) * 0.5;
    let off_y = (dst_f - src_hf * scale) * 0.5;
    let inv_scale = 1.0 / scale;
    let footprint_area = inv_scale * inv_scale;

    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(target_px, target_px);
    let out_slice = out.make_mut_slice();
    let stride = src_w as usize;

    for dy in 0..target_px {
        let sy0 = ((dy as f32 - off_y) * inv_scale).max(0.0);
        let sy1 = ((dy as f32 + 1.0 - off_y) * inv_scale).min(src_hf);
        if sy1 <= sy0 {
            continue;
        }
        let iy0 = (sy0.floor() as u32).min(src_h - 1);
        let iy1 = ((sy1.ceil() as u32).saturating_sub(1)).min(src_h - 1);

        for dx in 0..target_px {
            let sx0 = ((dx as f32 - off_x) * inv_scale).max(0.0);
            let sx1 = ((dx as f32 + 1.0 - off_x) * inv_scale).min(src_wf);
            if sx1 <= sx0 {
                continue;
            }
            let ix0 = (sx0.floor() as u32).min(src_w - 1);
            let ix1 = ((sx1.ceil() as u32).saturating_sub(1)).min(src_w - 1);

            let mut acc = [0.0_f32; 4];
            for iy in iy0..=iy1 {
                let oy = ((iy + 1) as f32).min(sy1) - (iy as f32).max(sy0);
                let row = iy as usize * stride;
                for ix in ix0..=ix1 {
                    let ox = ((ix + 1) as f32).min(sx1) - (ix as f32).max(sx0);
                    let w = ox * oy;
                    let p = linear[row + ix as usize];
                    acc[0] += p[0] * w;
                    acc[1] += p[1] * w;
                    acc[2] += p[2] * w;
                    acc[3] += p[3] * w;
                }
            }

            let out_a = (acc[3] / footprint_area).clamp(0.0, 1.0);
            if out_a <= 0.0 {
                continue;
            }
            let inv_a = 1.0 / acc[3];
            let r_srgb = linear_to_srgb((acc[0] * inv_a).clamp(0.0, 1.0));
            let g_srgb = linear_to_srgb((acc[1] * inv_a).clamp(0.0, 1.0));
            let b_srgb = linear_to_srgb((acc[2] * inv_a).clamp(0.0, 1.0));
            let a_u8 = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            let premul = |channel: f32| -> u8 {
                ((channel * out_a * 255.0).round() as u32).min(a_u8 as u32) as u8
            };
            out_slice[(dy * target_px + dx) as usize] = Rgba8Pixel::new(
                premul(r_srgb),
                premul(g_srgb),
                premul(b_srgb),
                a_u8,
            );
        }
    }

    Image::from_rgba8_premultiplied(out)
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::{ACCOUNT_COLORS, account_view, color_from_name, format_countdown, format_expiry, format_reset_clock, is_account_color, mask_account_name, model, plan_display_name, primary_window_index, relative_luminance, reset_timer_color, visible_usage_color};
    use crate::config::AppConfig;
    use crate::domain::{AccountRecord, RateWindow, ResetCredit, UsageSnapshot};
    use chrono::{Local, TimeZone};
    use slint::Model;
    use std::path::PathBuf;

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
    fn dark_account_colors_are_lifted_for_usage_bar_contrast() {
        let black = color_from_name("black");
        let visible = visible_usage_color(black);
        assert!(relative_luminance(visible) >= 0.24);
        assert!(visible.red() > black.red());

        let green = color_from_name("green");
        assert_eq!(visible_usage_color(green), green);
    }

    #[test]
    fn account_name_mask_keeps_only_a_hint() {
        assert_eq!(mask_account_name("Work"), "W•••");
        assert_eq!(mask_account_name(""), "");
    }

    #[test]
    fn plan_names_are_compact_and_unknown_values_remain_truthful() {
        assert_eq!(plan_display_name(Some("free")), "Free");
        assert_eq!(plan_display_name(Some("PLUS")), "Plus");
        assert_eq!(plan_display_name(Some("future_tier")), "Future Tier");
        assert_eq!(plan_display_name(None), "");
    }

    #[test]
    fn reset_timer_moves_from_red_toward_green() {
        let weekly_far = RateWindow {
            label: Some("Weekly".into()),
            used_percent: 0.0,
            duration_mins: Some(10_080),
            resets_at: Some(7 * 24 * 60 * 60),
        };
        let weekly_near = RateWindow { resets_at: Some(12 * 60 * 60), ..weekly_far.clone() };
        let short_far = RateWindow {
            label: Some("5-hour".into()),
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

    #[test]
    fn reset_credit_expiry_includes_the_exact_local_time() {
        let expiry = Local.with_ymd_and_hms(2026, 1, 15, 3, 4, 0).single().unwrap().timestamp();
        assert_eq!(format_expiry(expiry, expiry), "Expires Jan 15 at 03:04");
        assert_eq!(format_expiry(expiry, expiry - 1), "Expires Jan 15 at 03:04 · 1 day");
    }

    #[test]
    fn reset_clocks_use_exact_timestamps_in_the_selected_time_zone() {
        let zone = chrono::FixedOffset::west_opt(3 * 3_600).unwrap();
        let reset = zone.with_ymd_and_hms(2026, 10, 23, 11, 0, 0).unwrap().timestamp();
        assert_eq!(format_reset_clock(reset, reset - 3_600, false, &zone), "1h (11:00am)");
        assert_eq!(format_reset_clock(reset, reset - 3_600, true, &zone), "1h (11:00)");
        assert_eq!(format_reset_clock(reset, reset - 1, true, &zone), "1m (11:00)");
        assert_eq!(format_reset_clock(reset, reset - 3_600, true, &chrono::Utc), "1h (14:00)");
        assert_eq!(format_reset_clock(reset, reset - 6 * 86_400 - 23 * 3_600, false, &zone), "6d23h (11:00am)");
    }

    #[test]
    fn reset_clocks_handle_midnight_noon_and_expired_or_invalid_timestamps() {
        let noon = chrono::Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap().timestamp();
        let midnight = noon + 12 * 3_600;
        assert_eq!(format_reset_clock(noon, noon - 3_600, false, &chrono::Utc), "1h (12:00pm)");
        assert_eq!(format_reset_clock(midnight, midnight - 600, false, &chrono::Utc), "10m (12:00am)");
        assert_eq!(format_reset_clock(midnight, midnight - 600, true, &chrono::Utc), "10m (00:00)");
        assert_eq!(format_reset_clock(noon, noon, true, &chrono::Utc), "0m");
        assert_eq!(format_reset_clock(noon, noon + 1, false, &chrono::Utc), "0m");
        assert!(!format_reset_clock(i64::MAX, 0, false, &chrono::Utc).contains('('));
        assert_eq!(format_countdown(i64::MIN, i64::MAX), "0m");
    }

    #[test]
    fn banked_resets_can_be_hidden_without_leaving_empty_details() {
        let record = AccountRecord {
            id: "account".into(),
            home: PathBuf::new(),
            provider_id: "openai".into(),
            display_name: "Account".into(),
            color_name: "cyan".into(),
            enabled: true,
            pin_short: false,
            expanded: true,
            name_revealed: false,
            email_revealed: false,
            confirm_credit_id: String::new(),
            snapshot: Some(UsageSnapshot {
                email: None,
                plan_type: Some("free".into()),
                bucket_name: None,
                windows: Vec::new(),
                reset_available_count: 0,
                reset_credits: vec![ResetCredit {
                    id: Some("credit".into()),
                    title: None,
                    description: None,
                    expires_at: None,
                    status: Some("available".into()),
                }],
            }),
            last_error: None,
        };

        let shown_cfg = AppConfig::default();
        let hidden_cfg = AppConfig { show_banked_resets: false, ..AppConfig::default() };
        let shown = account_view(&record, 1, &shown_cfg, false);
        let hidden = account_view(&record, 1, &hidden_cfg, false);
        assert!(shown.has_reset_credits);
        assert_eq!(shown.reset_credits.row_count(), 1);
        assert!(shown.has_hidden_details);
        assert!(!hidden.has_reset_credits);
        assert_eq!(hidden.reset_credits.row_count(), 0);
        assert!(!hidden.has_hidden_details);
        assert!(shown.row_height_px > hidden.row_height_px);
    }

    #[test]
    fn antigravity_third_party_limits_can_be_hidden_without_affecting_gemini_windows() {
        let record = AccountRecord {
            id: "antigravity:test".into(),
            home: PathBuf::new(),
            provider_id: crate::antigravity::ID.into(),
            display_name: "Antigravity".into(),
            color_name: "red".into(),
            enabled: true,
            pin_short: false,
            expanded: true,
            name_revealed: false,
            email_revealed: false,
            confirm_credit_id: String::new(),
            snapshot: Some(UsageSnapshot {
                email: Some("user@example.com".into()),
                plan_type: Some("Google AI Pro".into()),
                bucket_name: Some("Antigravity".into()),
                windows: vec![
                    RateWindow { label: Some("5-hour".into()), used_percent: 20.0, duration_mins: Some(300), resets_at: None },
                    RateWindow { label: Some("Claude/GPT 5-hour".into()), used_percent: 0.0, duration_mins: Some(300), resets_at: None },
                    RateWindow { label: Some("Claude/GPT weekly".into()), used_percent: 0.0, duration_mins: Some(10_080), resets_at: None },
                    RateWindow { label: Some("Weekly".into()), used_percent: 10.0, duration_mins: Some(10_080), resets_at: None },
                ],
                reset_available_count: 0,
                reset_credits: Vec::new(),
            }),
            last_error: None,
        };

        let shown_cfg = AppConfig::default();
        let hidden_cfg = AppConfig { show_antigravity_third_party_limits: false, ..AppConfig::default() };
        let shown = account_view(&record, 1, &shown_cfg, false);
        let hidden = account_view(&record, 1, &hidden_cfg, false);
        assert_eq!(shown.detail_limits.row_count(), 2);
        assert_eq!(hidden.detail_limits.row_count(), 0);
        assert_eq!(hidden.long_label.as_str(), "Weekly");
        assert_eq!(hidden.short_label.as_str(), "5-hour");
        assert!(shown.row_height_px > hidden.row_height_px);
    }

    #[test]
    fn settings_height_is_independent_of_small_dashboard_account_counts() {
        let mut records = vec![AccountRecord {
            id: "only".into(), home: PathBuf::new(), provider_id: "openai".into(),
            display_name: "Only account".into(), color_name: "cyan".into(),
            enabled: true, pin_short: false, expanded: false,
            name_revealed: false, email_revealed: false,
            confirm_credit_id: String::new(), snapshot: None, last_error: None,
        }];
        let cfg = AppConfig::default();
        let dashboard_height = super::panel_height(&records, &cfg);
        assert!(dashboard_height < 520.0);
        assert_eq!(super::settings_panel_height(dashboard_height), 520.0);
        // Going back still uses the compact dashboard calculation.
        assert_eq!(super::panel_height(&records, &cfg), dashboard_height);
        records[0].enabled = false;
        assert_eq!(super::settings_panel_height(super::panel_height(&records, &cfg)), 520.0);
        assert_eq!(super::settings_panel_height(super::panel_height(&[], &cfg)), 520.0);
        assert_eq!(super::settings_panel_height(520.0), 520.0);
        assert_eq!(super::settings_panel_height(super::PANEL_MAX_HEIGHT), super::PANEL_MAX_HEIGHT);
    }

    #[test]
    fn separator_is_hidden_after_the_last_enabled_account() {
        let record = AccountRecord {
            id: "first".into(),
            home: PathBuf::new(),
            provider_id: "openai".into(),
            display_name: "First".into(),
            color_name: "cyan".into(),
            enabled: true,
            pin_short: false,
            expanded: false,
            name_revealed: false,
            email_revealed: false,
            confirm_credit_id: String::new(),
            snapshot: None,
            last_error: None,
        };
        let mut last_enabled = record.clone();
        last_enabled.id = "last-enabled".into();
        let mut disabled_tail = record.clone();
        disabled_tail.id = "disabled-tail".into();
        disabled_tail.enabled = false;

        let cfg = AppConfig::default();
        let (rows, enabled_count) = model(&[record, last_enabled, disabled_tail], &cfg);

        assert_eq!(enabled_count, 2);
        assert!(rows.row_data(0).expect("first row").show_separator);
        assert!(!rows.row_data(1).expect("last enabled row").show_separator);
        assert!(!rows.row_data(2).expect("disabled row").show_separator);
    }

    #[test]
    fn provider_accents_follow_enabled_counts_for_each_icon() {
        let codex = AccountRecord {
            id: "codex".into(), home: PathBuf::new(), provider_id: "openai".into(),
            display_name: "Codex".into(), color_name: "cyan".into(),
            enabled: true, pin_short: false, expanded: false,
            name_revealed: false, email_revealed: false,
            confirm_credit_id: String::new(), snapshot: None, last_error: None,
        };
        let second_codex = AccountRecord {
            id: "second-codex".into(), enabled: false, ..codex.clone()
        };
        let claude = AccountRecord {
            id: "claude".into(), provider_id: "anthropic".into(), ..codex.clone()
        };
        let mut records = [codex, second_codex, claude];

        let cfg = AppConfig::default();
        let (rows, _) = model(&records, &cfg);
        assert!(rows.iter().all(|row| !row.show_accent));

        records[1].enabled = true;
        let (rows, _) = model(&records, &cfg);
        assert!(rows.row_data(0).unwrap().show_accent);
        assert!(rows.row_data(1).unwrap().show_accent);
        assert!(!rows.row_data(2).unwrap().show_accent);
    }

    #[test]
    fn model_buckets_surface_the_most_constrained_quota() {
        let comfortable = RateWindow {
            label: Some("Gemini Flash".into()),
            used_percent: 20.0,
            duration_mins: None,
            resets_at: None,
        };
        let constrained = RateWindow {
            label: Some("Gemini Pro".into()),
            used_percent: 85.0,
            duration_mins: None,
            resets_at: None,
        };
        assert_eq!(primary_window_index(&[&comfortable, &constrained]), Some(1));
    }

    #[test]
    fn icon_resampler_produces_antialiased_square_output_preserving_aspect_ratio() {
        use super::resample_icon_square;
        use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

        // 40x80 white vertical bar (1:2 aspect ratio) centered into 20x20 -> 10x20 centered at x=5..15.
        let mut src = SharedPixelBuffer::<Rgba8Pixel>::new(40, 80);
        for pixel in src.make_mut_slice() {
            *pixel = Rgba8Pixel::new(255, 255, 255, 255);
        }
        let resampled = resample_icon_square(&Image::from_rgba8_premultiplied(src), 20);
        let out = resampled.to_rgba8_premultiplied().expect("premultiplied output");
        assert_eq!((out.width(), out.height()), (20, 20));
        let slice = out.as_slice();
        // Left margin is transparent, center is opaque white.
        assert_eq!(slice[0].a, 0);
        assert_eq!(slice[10 * 20 + 10], Rgba8Pixel::new(255, 255, 255, 255));

        // Checkerboard 4x4 downsampled to 2x2 should produce smooth 50% alpha instead of binary 0/255.
        let mut checker = SharedPixelBuffer::<Rgba8Pixel>::new(4, 4);
        let checker_slice = checker.make_mut_slice();
        for y in 0..4 {
            for x in 0..4 {
                if (x + y) % 2 == 0 {
                    checker_slice[y * 4 + x] = Rgba8Pixel::new(255, 255, 255, 255);
                }
            }
        }
        let smooth = resample_icon_square(&Image::from_rgba8_premultiplied(checker), 2)
            .to_rgba8_premultiplied()
            .expect("smooth output");
        assert_eq!((smooth.width(), smooth.height()), (2, 2));
        for pixel in smooth.as_slice() {
            assert!(pixel.a > 100 && pixel.a < 160, "expected ~50% anti-aliased alpha, got {}", pixel.a);
        }
    }
}
