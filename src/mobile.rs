use crate::WorkerCommand;
use crate::config::{AppConfig, MobileConfig};
use crate::domain::{AccountRecord, RateWindow};
use axum::extract::{Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HeaderName, LOCATION, SET_COOKIE,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;

const COOKIE_NAME: &str = "agents_usage_mobile";
const REFERRER_POLICY: HeaderName = HeaderName::from_static("referrer-policy");
const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");
const INDEX_HTML: &str = include_str!("../mobile/index.html");
const APP_CSS: &str = include_str!("../mobile/app.css");
const APP_JS: &str = include_str!("../mobile/app.js");
const MANIFEST: &str = include_str!("../mobile/manifest.webmanifest");
const SERVICE_WORKER: &str = include_str!("../mobile/sw.js");
const ICON_SVG: &str = include_str!("../packaging/linux/agents-usage.svg");
const ICON_192: &[u8] = include_bytes!("../mobile/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../mobile/icon-512.png");

#[derive(Clone)]
struct MobileServerState {
    access_token: String,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    refreshing: Arc<AtomicBool>,
    tx: UnboundedSender<WorkerCommand>,
}

#[derive(Deserialize)]
struct PairQuery {
    token: Option<String>,
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
    pin_short_global: bool,
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(access_token) = mobile_config.access_token.filter(|token| token.len() >= 32) else {
        return Err("mobile access is enabled but its access token is missing or too short".into());
    };

    let state = MobileServerState {
        access_token,
        accounts,
        config,
        refreshing,
        tx,
    };
    let routes = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.css", get(styles))
        .route("/app.js", get(script))
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(service_worker))
        .route("/icon.svg", get(icon_svg))
        .route("/icon-192.png", get(icon_192))
        .route("/icon-512.png", get(icon_512))
        .route("/pair", get(pair))
        .route("/api/state", get(api_state))
        .route("/api/refresh", post(api_refresh))
        .with_state(state);
    // The second mount also permits a direct reverse proxy that preserves its
    // path prefix. Tailscale Serve normally strips the prefix before proxying.
    let app = Router::new()
        .merge(routes.clone())
        .nest("/agents-usage", routes);
    let address = format!("{}:{}", mobile_config.bind, mobile_config.port);
    let listener = TcpListener::bind(&address).await?;
    eprintln!("mobile: companion view listening on http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Response {
    static_response(
        INDEX_HTML.as_bytes(),
        "text/html; charset=utf-8",
        "no-store",
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
    if !constant_time_eq(token.as_bytes(), state.access_token.as_bytes()) {
        return unauthorized();
    }

    let secure = if forwarded_over_https(&headers) { "; Secure" } else { "" };
    let cookie = format!(
        "{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000{secure}",
        state.access_token
    );
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
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
    if !authorized(&headers, &state.access_token) {
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
        pin_short_global: config.pin_short_global,
        accounts,
    };
    let mut response = Json(snapshot).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_security_headers(response.headers_mut());
    response
}

async fn api_refresh(State(state): State<MobileServerState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.access_token) {
        return unauthorized();
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

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|cookies| {
            cookies.split(';').any(|cookie| {
                let Some((name, value)) = cookie.trim().split_once('=') else {
                    return false;
                };
                name == COOKIE_NAME && constant_time_eq(value.as_bytes(), expected.as_bytes())
            })
        })
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
    use super::{constant_time_eq, forwarded_over_https};
    use axum::http::{HeaderMap, HeaderValue};

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
}
