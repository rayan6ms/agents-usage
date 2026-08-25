use crate::WorkerCommand;
use crate::config;
use crate::config::{AppConfig, MobileConfig, MobileDevice, MobilePairing};
use crate::domain::{AccountRecord, RateWindow};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HeaderName, LOCATION, SET_COOKIE,
    RETRY_AFTER, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

const COOKIE_NAME: &str = "agents_usage_mobile";
const PAIRING_TTL_SECONDS: i64 = 10 * 60;
// Android requires a finite Max-Age even though desktop-side sessions remain
// active until explicitly revoked. Ten years avoids routine re-pairing.
const SESSION_COOKIE_MAX_AGE_SECONDS: i64 = 10 * 365 * 24 * 60 * 60;
const FORCE_REFRESH_COOLDOWN: Duration = Duration::from_secs(2);
const REFERRER_POLICY: HeaderName = HeaderName::from_static("referrer-policy");
const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");
const INDEX_HTML: &str = include_str!("../mobile/index.html");
const APP_CSS: &str = include_str!("../mobile/app.css");
const APP_JS: &str = include_str!("../mobile/app.js");
const MANIFEST: &str = include_str!("../mobile/manifest.webmanifest");
const SERVICE_WORKER: &str = include_str!("../mobile/sw.js");
const ICON_SVG: &str = include_str!("../packaging/linux/agents-usage.svg");
const PROVIDER_OPENAI: &str = include_str!("../assets/providers/openai.svg");
const PROVIDER_OPENCODE: &str = include_str!("../assets/providers/opencode.svg");
const PROVIDER_ANTHROPIC: &str = include_str!("../assets/providers/anthropic.svg");
const PROVIDER_GOOGLE: &str = include_str!("../assets/providers/gemini.svg");
const PROVIDER_CURSOR: &str = include_str!("../assets/providers/cursor.svg");
const PROVIDER_XAI: &str = include_str!("../assets/providers/xai.svg");
const ICON_192: &[u8] = include_bytes!("../mobile/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../mobile/icon-512.png");

#[derive(Clone)]
struct MobileServerState {
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    refreshing: Arc<AtomicBool>,
    tx: UnboundedSender<WorkerCommand>,
    persist_config: bool,
    last_forced_refresh: Arc<Mutex<Option<Instant>>>,
}

#[derive(Deserialize)]
struct PairQuery {
    token: Option<String>,
    path: Option<String>,
    device: Option<String>,
    device_id: Option<String>,
}

#[derive(Serialize)]
struct MobileSnapshot {
    server_time: i64,
    refreshing: bool,
    blur_names: bool,
    blur_emails: bool,
    color_reset_timers: bool,
    usage_bar_color_mode: String,
    usage_bar_custom_color: String,
    always_show_reset_counter: bool,
    show_banked_resets: bool,
    accounts: Vec<MobileAccount>,
}

#[derive(Serialize)]
struct MobileAccount {
    key: usize,
    provider_id: String,
    display_name: String,
    masked_display_name: String,
    color: String,
    email: String,
    masked_email: String,
    pin_short: bool,
    expanded: bool,
    error: Option<String>,
    windows: Vec<RateWindow>,
    reset_available_count: u32,
    reset_credits: Vec<MobileResetCredit>,
}

#[derive(Serialize)]
struct MobileResetCredit {
    title: String,
    description: String,
    expires_at: Option<i64>,
}

pub async fn serve(
    mobile_config: MobileConfig,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    refreshing: Arc<AtomicBool>,
    tx: UnboundedSender<WorkerCommand>,
    shutdown: oneshot::Receiver<()>,
    ready: oneshot::Sender<Result<String, String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = MobileServerState {
        accounts,
        config,
        refreshing,
        tx,
        persist_config: true,
        last_forced_refresh: Arc::new(Mutex::new(None)),
    };
    let app = router(state);
    let address = match parse_bind_address(&mobile_config.bind, mobile_config.port) {
        Ok(address) => address,
        Err(error) => {
            let _ = ready.send(Err(format!("Invalid listen address: {error}")));
            return Err(error.into());
        }
    };
    let listener = match TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = ready.send(Err(format!("Could not listen on {address}: {error}")));
            return Err(error.into());
        }
    };
    let _ = ready.send(Ok(format!("Listening on {address}")));
    eprintln!("mobile: companion view listening on http://{address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await?;
    Ok(())
}

fn router(state: MobileServerState) -> Router {
    let routes = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.css", get(styles))
        .route("/app.js", get(script))
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(service_worker))
        .route("/icon.svg", get(icon_svg))
        .route("/provider-icons/{provider}", get(provider_icon))
        .route("/icon-192.png", get(icon_192))
        .route("/icon-512.png", get(icon_512))
        .route("/pair", get(pair))
        .route("/api/health", get(api_health))
        .route("/api/state", get(api_state))
        .route("/api/refresh", post(api_refresh))
        .route("/api/refresh-if-stale", post(api_refresh_if_stale))
        .with_state(state);
    // The second mount also permits a direct reverse proxy that preserves its
    // path prefix. Tailscale Serve normally strips the prefix before proxying.
    Router::new()
        .merge(routes.clone())
        .nest("/agents-usage", routes)
}

async fn index() -> Response {
    static_response(
        INDEX_HTML.as_bytes(),
        "text/html; charset=utf-8",
        // The shell contains no account data. Keeping it in WebView's HTTP
        // cache lets the Android app reopen the separately persisted last
        // state when the desktop is offline, including on cleartext LAN origins
        // where service workers are unavailable.
        "private, max-age=3600",
    )
}

async fn styles() -> Response {
    static_response(
        APP_CSS.as_bytes(),
        "text/css; charset=utf-8",
        "public, max-age=3600",
    )
}

async fn script() -> Response {
    static_response(
        APP_JS.as_bytes(),
        "text/javascript; charset=utf-8",
        "public, max-age=3600",
    )
}

async fn manifest() -> Response {
    static_response(
        MANIFEST.as_bytes(),
        "application/manifest+json",
        "public, max-age=3600",
    )
}

async fn service_worker() -> Response {
    static_response(
        SERVICE_WORKER.as_bytes(),
        "text/javascript; charset=utf-8",
        "no-cache",
    )
}

async fn icon_svg() -> Response {
    static_response(
        ICON_SVG.as_bytes(),
        "image/svg+xml",
        "public, max-age=86400",
    )
}

async fn provider_icon(AxumPath(provider): AxumPath<String>) -> Response {
    let icon = match provider.as_str() {
        "openai" => PROVIDER_OPENAI,
        "opencode" => PROVIDER_OPENCODE,
        "anthropic" => PROVIDER_ANTHROPIC,
        "google" => PROVIDER_GOOGLE,
        "cursor" => PROVIDER_CURSOR,
        "xai" => PROVIDER_XAI,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    static_response(icon.as_bytes(), "image/svg+xml", "public, max-age=86400")
}

async fn icon_192() -> Response {
    static_response(ICON_192, "image/png", "public, max-age=86400")
}

async fn icon_512() -> Response {
    static_response(ICON_512, "image/png", "public, max-age=86400")
}

async fn pair(
    State(state): State<MobileServerState>,
    headers: HeaderMap,
    Query(query): Query<PairQuery>,
) -> Response {
    let Some(token) = query.token else {
        return unauthorized();
    };
    let now = chrono::Utc::now().timestamp();
    let supplied_hash = hash_token(&token);
    let session_token = new_token();
    let session_hash = hash_token(&session_token);
    let device_name = normalized_device_name(query.device.as_deref());
    let installation_id = normalized_installation_id(query.device_id.as_deref());
    let Ok(mut current_config) = state.config.lock() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Desktop configuration is unavailable.").into_response();
    };
    let mut next_config = current_config.clone();
    let pairing_matches = |pairing: &MobilePairing| {
        pairing.expires_at >= now
            && pairing.remaining_uses > 0
            && constant_time_eq(pairing.token_hash.as_bytes(), supplied_hash.as_bytes())
    };
    let mut pairing_valid = next_config.mobile.pairing.as_ref().is_some_and(pairing_matches);
    if !pairing_valid {
        let disk_config = config::load();
        if disk_config.mobile.enabled
            && disk_config.mobile.pairing.as_ref().is_some_and(pairing_matches)
        {
            next_config.mobile.pairing = disk_config.mobile.pairing;
            pairing_valid = true;
        }
    }
    if !pairing_valid {
        return unauthorized();
    }
    next_config.mobile.devices.retain(|device| device_is_active(device, now));
    deduplicate_devices(&mut next_config.mobile.devices);
    let device_id = next_config
        .mobile
        .pairing
        .as_ref()
        .and_then(|pairing| pairing.device_id.clone())
        .or_else(|| {
            installation_id.as_ref().and_then(|installation_id| {
                next_config
                    .mobile
                    .devices
                    .iter()
                    .find(|device| device.installation_id.as_ref() == Some(installation_id))
                    .map(|device| device.id.clone())
            })
        })
        .or_else(|| {
            next_config
                .mobile
                .devices
                .iter()
                .filter(|device| device.installation_id.is_none() && device.name.eq_ignore_ascii_case(&device_name))
                .max_by_key(|device| device.last_seen_at.unwrap_or(device.created_at))
                .map(|device| device.id.clone())
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if let Some(pairing) = next_config.mobile.pairing.as_mut() {
        pairing.remaining_uses = pairing.remaining_uses.saturating_sub(1);
        pairing.device_id = Some(device_id.clone());
        if pairing.remaining_uses == 0 {
            next_config.mobile.pairing = None;
        }
    }
    if let Some(device) = next_config.mobile.devices.iter_mut().find(|device| device.id == device_id) {
        if device.token_hash != session_hash && !device.additional_token_hashes.contains(&session_hash) {
            device.additional_token_hashes.push(session_hash);
        }
        device.name = device_name;
        if installation_id.is_some() {
            device.installation_id = installation_id;
        }
        device.expires_at = 0;
        device.last_seen_at = Some(now);
    } else {
        next_config.mobile.devices.push(MobileDevice {
            id: device_id,
            name: device_name,
            installation_id,
            token_hash: session_hash,
            additional_token_hashes: Vec::new(),
            created_at: now,
            expires_at: 0,
            last_seen_at: Some(now),
        });
    }
    if state.persist_config && config::save(&next_config).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Could not save the paired device.").into_response();
    }
    *current_config = next_config;
    drop(current_config);
    let _ = state.tx.send(WorkerCommand::MobileDeviceListChanged);

    let cookie_path = normalized_cookie_path(query.path.as_deref());
    let secure = if forwarded_over_https(&headers) { "; Secure" } else { "" };
    let cookie = format!(
        "{COOKIE_NAME}={session_token}; Path={cookie_path}; HttpOnly; SameSite=Strict; Max-Age={SESSION_COOKIE_MAX_AGE_SECONDS}{secure}"
    );
    let Ok(cookie) = HeaderValue::from_str(&cookie) else {
        return (StatusCode::BAD_REQUEST, "The requested cookie path is invalid.").into_response();
    };
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    // A relative redirect preserves a Tailscale Serve path prefix.
    response
        .headers_mut()
        .insert(LOCATION, HeaderValue::from_static("./"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

async fn api_state(State(state): State<MobileServerState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.config) {
        return unauthorized();
    }
    let config = state
        .config
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    let accounts = state
        .accounts
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    let accounts = accounts
        .into_iter()
        .enumerate()
        .filter(|(_, account)| account.enabled)
        .map(|(key, account)| mobile_account(key, account))
        .collect();
    let snapshot = MobileSnapshot {
        server_time: chrono::Utc::now().timestamp(),
        refreshing: state.refreshing.load(Ordering::SeqCst),
        blur_names: config.blur_names,
        blur_emails: config.blur_emails,
        color_reset_timers: config.color_reset_timers,
        usage_bar_color_mode: config.usage_bar_color_mode.as_str().into(),
        usage_bar_custom_color: config.usage_bar_custom_color,
        always_show_reset_counter: config.always_show_reset_counter,
        show_banked_resets: config.show_banked_resets,
        accounts,
    };
    let mut response = Json(snapshot).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

async fn api_health(State(state): State<MobileServerState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.config) {
        return unauthorized();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

async fn api_refresh(State(state): State<MobileServerState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.config) {
        return unauthorized();
    }
    if let Ok(mut last_refresh) = state.last_forced_refresh.lock() {
        if last_refresh.is_some_and(|when| when.elapsed() < FORCE_REFRESH_COOLDOWN) {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                "Wait briefly before refreshing again.",
            )
                .into_response();
            response.headers_mut().insert(RETRY_AFTER, HeaderValue::from_static("2"));
            response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            insert_security_headers(response.headers_mut());
            return response;
        }
        *last_refresh = Some(Instant::now());
    }
    if state.tx.send(WorkerCommand::Refresh).is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "desktop worker unavailable",
        )
            .into_response();
    }
    let mut response = StatusCode::ACCEPTED.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

async fn api_refresh_if_stale(State(state): State<MobileServerState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.config) {
        return unauthorized();
    }
    if state.tx.send(WorkerCommand::RefreshIfStaleMobile).is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "desktop worker unavailable").into_response();
    }
    let mut response = StatusCode::ACCEPTED.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

fn forwarded_over_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

fn mobile_account(key: usize, account: AccountRecord) -> MobileAccount {
    let snapshot = account
        .snapshot
        .unwrap_or_else(|| crate::domain::UsageSnapshot {
            email: None,
            bucket_name: None,
            windows: Vec::new(),
            reset_available_count: 0,
            reset_credits: Vec::new(),
        });
    let email = snapshot.email.clone().unwrap_or_default();
    let reset_credits = snapshot
        .reset_credits
        .iter()
        .filter(|credit| {
            credit
                .status
                .as_deref()
                .map(|status| status == "available")
                .unwrap_or(true)
        })
        .map(|credit| MobileResetCredit {
            title: credit
                .title
                .clone()
                .unwrap_or_else(|| "Rate-limit reset".into()),
            description: credit.description.clone().unwrap_or_default(),
            expires_at: credit.expires_at,
        })
        .collect();
    MobileAccount {
        key,
        provider_id: account.provider_id,
        masked_display_name: crate::ui_model::mask_account_name(&account.display_name),
        display_name: account.display_name,
        color: account.color_name,
        masked_email: crate::ui_model::mask_email(&email),
        email,
        pin_short: account.pin_short,
        expanded: account.expanded,
        error: account.last_error,
        windows: snapshot.windows,
        reset_available_count: snapshot.reset_available_count,
        reset_credits,
    }
}

fn authorized(headers: &HeaderMap, config: &Arc<Mutex<AppConfig>>) -> bool {
    let Some(token) = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == COOKIE_NAME).then_some(value.to_string())
            })
        })
    else {
        return false;
    };
    let supplied_hash = hash_token(&token);
    let now = chrono::Utc::now().timestamp();
    let Ok(mut config) = config.lock() else { return false; };
    if let Some(device) = config.mobile.devices.iter_mut().find(|device| {
            device_is_active(device, now)
                && std::iter::once(&device.token_hash)
                    .chain(device.additional_token_hashes.iter())
                    .any(|expected| constant_time_eq(expected.as_bytes(), supplied_hash.as_bytes()))
        }) {
        if device.last_seen_at.is_none_or(|last_seen| now - last_seen >= 60) {
            device.last_seen_at = Some(now);
        }
        true
    } else {
        false
    }
}

pub fn create_pairing(config: &mut AppConfig, uses: u8) -> String {
    let token = new_token();
    config.mobile.pairing = Some(MobilePairing {
        token_hash: hash_token(&token),
        expires_at: chrono::Utc::now().timestamp() + PAIRING_TTL_SECONDS,
        remaining_uses: uses.clamp(1, 4),
        device_id: None,
    });
    token
}

pub fn migrate_legacy_access(config: &mut AppConfig) -> bool {
    let Some(token) = config.mobile.access_token.take().filter(|value| value.len() >= 32) else {
        config.mobile.access_token = None;
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    config.mobile.devices.push(MobileDevice {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Previously paired phone".into(),
        installation_id: None,
        token_hash: hash_token(&token),
        additional_token_hashes: Vec::new(),
        created_at: now,
        expires_at: 0,
        last_seen_at: None,
    });
    true
}

pub fn make_device_sessions_persistent(config: &mut AppConfig) -> bool {
    let now = chrono::Utc::now().timestamp();
    let previous_len = config.mobile.devices.len();
    config.mobile.devices.retain(|device| device_is_active(device, now));
    let mut changed = config.mobile.devices.len() != previous_len;
    for device in &mut config.mobile.devices {
        if device.expires_at != 0 {
            device.expires_at = 0;
            changed = true;
        }
    }
    changed |= deduplicate_devices(&mut config.mobile.devices);
    changed
}

fn deduplicate_devices(devices: &mut Vec<MobileDevice>) -> bool {
    let original = devices.clone();
    devices.sort_by_key(|device| {
        std::cmp::Reverse((device.last_seen_at.unwrap_or(device.created_at), device.created_at))
    });

    let mut consolidated: Vec<MobileDevice> = Vec::with_capacity(devices.len());
    for device in std::mem::take(devices) {
        let duplicate = consolidated.iter_mut().find(|existing| {
            match (&existing.installation_id, &device.installation_id) {
                (Some(left), Some(right)) => left == right,
                (None, None) => existing.name.trim().eq_ignore_ascii_case(device.name.trim()),
                _ => false,
            }
        });
        if let Some(existing) = duplicate {
            merge_device_tokens(existing, &device);
        } else {
            consolidated.push(device);
        }
    }
    *devices = consolidated;
    *devices != original
}

fn merge_device_tokens(target: &mut MobileDevice, source: &MobileDevice) {
    for token_hash in std::iter::once(&source.token_hash).chain(source.additional_token_hashes.iter()) {
        if token_hash != &target.token_hash && !target.additional_token_hashes.contains(token_hash) {
            target.additional_token_hashes.push(token_hash.clone());
        }
    }
}

pub fn device_is_active(device: &MobileDevice, now: i64) -> bool {
    device.expires_at == 0 || device.expires_at >= now
}

pub fn revoke_device(config: &mut AppConfig, id: &str) -> bool {
    let previous = config.mobile.devices.len();
    config.mobile.devices.retain(|device| device.id != id);
    previous != config.mobile.devices.len()
}

pub fn revoke_all_devices(config: &mut AppConfig) {
    config.mobile.devices.clear();
    config.mobile.pairing = None;
    config.mobile.access_token = None;
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn normalized_device_name(value: Option<&str>) -> String {
    let value = value.unwrap_or("Android phone").trim();
    let sanitized = value
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect::<String>();
    if sanitized.is_empty() { "Android phone".into() } else { sanitized }
}

fn normalized_installation_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (value.len() >= 16
        && value.len() <= 64
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        .then(|| value.to_ascii_lowercase())
}

fn normalized_cookie_path(value: Option<&str>) -> String {
    let Some(path) = value else { return "/".into(); };
    if !path.starts_with('/')
        || !path.ends_with('/')
        || path.contains("..")
        || path.bytes().any(|byte| !byte.is_ascii() || byte <= 0x20 || byte == 0x7f || byte == b';')
    {
        return "/".into();
    }
    path.into()
}

fn parse_bind_address(bind: &str, port: u16) -> Result<std::net::SocketAddr, std::net::AddrParseError> {
    if let Ok(address) = bind.parse::<std::net::IpAddr>() {
        return Ok(std::net::SocketAddr::new(address, port));
    }
    format!("{bind}:{port}").parse()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        "Pair this device from the desktop first.",
    )
        .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

fn insert_security_headers(headers: &mut HeaderMap) {
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        PERMISSIONS_POLICY,
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
}

fn static_response(
    body: &'static [u8],
    content_type: &'static str,
    cache_control: &'static str,
) -> Response {
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
    response.headers_mut().insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
    );
    insert_security_headers(response.headers_mut());
    response
}

#[cfg(test)]
mod tests {
    use super::{
        MobileServerState, constant_time_eq, create_pairing, device_is_active,
        forwarded_over_https, make_device_sessions_persistent, migrate_legacy_access,
        normalized_cookie_path, parse_bind_address, router,
    };
    use crate::WorkerCommand;
    use crate::config::AppConfig;
    use axum::body::Body;
    use axum::http::header::{CACHE_CONTROL, RETRY_AFTER, SET_COOKIE};
    use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::unbounded_channel;
    use tower::ServiceExt;

    #[test]
    fn constant_time_comparison_rejects_different_tokens() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn secure_cookie_transport_is_detected_from_reverse_proxy_header() {
        let mut headers = HeaderMap::new();
        assert!(!forwarded_over_https(&headers));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(forwarded_over_https(&headers));
    }

    #[test]
    fn ipv4_and_ipv6_bind_addresses_are_supported() {
        assert_eq!(parse_bind_address("0.0.0.0", 3765).unwrap().to_string(), "0.0.0.0:3765");
        assert_eq!(parse_bind_address("::", 3765).unwrap().to_string(), "[::]:3765");
    }

    #[test]
    fn cookie_paths_reject_header_injection_and_traversal() {
        assert_eq!(normalized_cookie_path(Some("/agents-usage/")), "/agents-usage/");
        assert_eq!(normalized_cookie_path(Some("/bad\r\npath/")), "/");
        assert_eq!(normalized_cookie_path(Some("/../private/")), "/");
        assert_eq!(normalized_cookie_path(Some("relative/")), "/");
    }

    #[test]
    fn legacy_master_token_becomes_an_individual_device_session() {
        let mut config = AppConfig::default();
        config.mobile.access_token = Some("a".repeat(64));
        assert!(migrate_legacy_access(&mut config));
        assert!(config.mobile.access_token.is_none());
        assert_eq!(config.mobile.devices.len(), 1);
        assert!(!config.mobile.devices[0].token_hash.contains(&"a".repeat(32)));
        assert_eq!(config.mobile.devices[0].expires_at, 0);
    }

    #[test]
    fn existing_device_sessions_become_persistent_and_remain_active() {
        let mut config = AppConfig::default();
        let now = chrono::Utc::now().timestamp();
        config.mobile.devices.push(crate::config::MobileDevice {
            id: "phone".into(),
            name: "Phone".into(),
            installation_id: None,
            token_hash: "hash".into(),
            additional_token_hashes: Vec::new(),
            created_at: now - 100,
            expires_at: now + 100,
            last_seen_at: None,
        });
        assert!(make_device_sessions_persistent(&mut config));
        assert_eq!(config.mobile.devices[0].expires_at, 0);
        assert!(device_is_active(&config.mobile.devices[0], now));
        assert!(!make_device_sessions_persistent(&mut config));
    }

    #[test]
    fn expired_device_sessions_are_not_revived() {
        let mut config = AppConfig::default();
        let now = chrono::Utc::now().timestamp();
        config.mobile.devices.push(crate::config::MobileDevice {
            id: "old-phone".into(),
            name: "Old phone".into(),
            installation_id: None,
            token_hash: "hash".into(),
            additional_token_hashes: Vec::new(),
            created_at: now - 200,
            expires_at: now - 100,
            last_seen_at: None,
        });
        assert!(make_device_sessions_persistent(&mut config));
        assert!(config.mobile.devices.is_empty());
    }

    #[test]
    fn historical_duplicate_phones_keep_the_most_recent_record_and_all_sessions() {
        let mut config = AppConfig::default();
        let now = chrono::Utc::now().timestamp();
        config.mobile.devices.extend([
            crate::config::MobileDevice {
                id: "older".into(),
                name: "Google Pixel".into(),
                installation_id: None,
                token_hash: "older-primary".into(),
                additional_token_hashes: vec!["older-route".into()],
                created_at: now - 200,
                expires_at: 0,
                last_seen_at: Some(now - 100),
            },
            crate::config::MobileDevice {
                id: "newer".into(),
                name: "google pixel".into(),
                installation_id: None,
                token_hash: "newer-primary".into(),
                additional_token_hashes: Vec::new(),
                created_at: now - 50,
                expires_at: 0,
                last_seen_at: Some(now - 10),
            },
        ]);

        assert!(make_device_sessions_persistent(&mut config));
        assert_eq!(config.mobile.devices.len(), 1);
        let device = &config.mobile.devices[0];
        assert_eq!(device.id, "newer");
        assert!(device.additional_token_hashes.contains(&"older-primary".into()));
        assert!(device.additional_token_hashes.contains(&"older-route".into()));
    }

    #[tokio::test]
    async fn pairing_is_one_time_and_issues_a_prefix_scoped_device_session() {
        let mut config = AppConfig::default();
        config.mobile.enabled = true;
        config.always_show_reset_counter = true;
        let pairing_token = create_pairing(&mut config, 1);
        let config = Arc::new(Mutex::new(config));
        let (tx, mut rx) = unbounded_channel::<WorkerCommand>();
        let app = router(MobileServerState {
            accounts: Arc::new(Mutex::new(Vec::new())),
            config: config.clone(),
            refreshing: Arc::new(AtomicBool::new(false)),
            tx,
            persist_config: false,
            last_forced_refresh: Arc::new(Mutex::new(None)),
        });

        let shell = app
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(shell.status(), StatusCode::OK);
        assert_eq!(
            shell.headers().get(CACHE_CONTROL).unwrap(),
            "private, max-age=3600"
        );

        let unauthenticated = app
            .clone()
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        for provider in ["openai", "opencode", "anthropic", "google", "cursor", "xai"] {
            let icon = app
                .clone()
                .oneshot(
                    Request::get(format!("/provider-icons/{provider}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(icon.status(), StatusCode::OK);
            assert_eq!(icon.headers().get("content-type").unwrap(), "image/svg+xml");
        }

        let uri = format!(
            "/agents-usage/pair?token={pairing_token}&path=/agents-usage/&device=Pixel%20test&device_id=11111111-2222-4333-8444-555555555555"
        );
        let paired = app
            .clone()
            .oneshot(
                Request::get(&uri)
                    .header("x-forwarded-proto", "https")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(paired.status(), StatusCode::SEE_OTHER);
        assert!(matches!(rx.recv().await, Some(WorkerCommand::MobileDeviceListChanged)));
        let set_cookie = paired.headers().get(SET_COOKIE).unwrap().to_str().unwrap();
        assert!(set_cookie.contains("Path=/agents-usage/"));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(set_cookie.contains("Secure"));
        let cookie = set_cookie.split(';').next().unwrap();

        let healthy = app
            .clone()
            .oneshot(
                Request::get("/agents-usage/api/health")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(healthy.status(), StatusCode::NO_CONTENT);
        assert_eq!(config.lock().unwrap().mobile.devices[0].name, "Pixel test");

        let state = app
            .clone()
            .oneshot(
                Request::get("/agents-usage/api/state")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(state.status(), StatusCode::OK);
        let state_body = String::from_utf8(state.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(!state_body.contains("token_hash"));
        assert!(!state_body.contains("codex_executable"));
        assert!(!state_body.contains("additional_codex_homes"));
        assert!(state_body.contains("\"always_show_reset_counter\":true"));
        assert!(state_body.contains("\"show_banked_resets\":true"));
        assert!(!state_body.contains("pin_short_global"));

        let next_pairing_token = {
            let mut config = config.lock().unwrap();
            create_pairing(&mut config, 1)
        };
        let repaired = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/agents-usage/pair?token={next_pairing_token}&path=/agents-usage/&device=Pixel%20test&device_id=11111111-2222-4333-8444-555555555555"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(repaired.status(), StatusCode::SEE_OTHER);
        assert!(matches!(rx.recv().await, Some(WorkerCommand::MobileDeviceListChanged)));
        assert_eq!(config.lock().unwrap().mobile.devices.len(), 1);

        let stale_refresh = app
            .clone()
            .oneshot(
                Request::post("/agents-usage/api/refresh-if-stale")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale_refresh.status(), StatusCode::ACCEPTED);
        assert!(matches!(rx.recv().await, Some(WorkerCommand::RefreshIfStaleMobile)));

        let manual_refresh = app
            .clone()
            .oneshot(
                Request::post("/agents-usage/api/refresh")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(manual_refresh.status(), StatusCode::ACCEPTED);
        assert!(matches!(rx.recv().await, Some(WorkerCommand::Refresh)));
        let rate_limited = app
            .clone()
            .oneshot(
                Request::post("/agents-usage/api/refresh")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rate_limited.headers().get(RETRY_AFTER).unwrap(), "2");

        let reused = app
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
    }
}
