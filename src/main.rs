#![allow(clippy::collapsible_if)] // Nested UI/state guards stay clearer than chained lock patterns.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod codex;
mod config;
mod discovery;
mod domain;
mod mobile;
mod providers;
mod ui_model;

use crate::config::{AppConfig, AccountPreference, UsageBarColorMode};
use crate::domain::{AccountRecord, CachedUsage, PendingReset, UsageSnapshot};
use copypasta::{ClipboardContext, ClipboardProvider};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;

slint::include_modules!();

#[cfg(target_os = "linux")]
const DBUS_NAME: &str = "io.github.agentsusagetray.App";
#[cfg(target_os = "linux")]
const DBUS_PATH: &str = "/io/github/agentsusagetray/App";
const PANEL_GAP_PX: i32 = 6;
const SCREEN_MARGIN_PX: i32 = 5;
const OPEN_REFRESH_FRESHNESS: Duration = Duration::from_secs(5);
const MOBILE_REFRESH_FRESHNESS: Duration = Duration::from_secs(30);
const MAX_RPC_CONCURRENCY: usize = 8;
const INTERACTIVE_REFRESH_CONCURRENCY: usize = 8;
const INTERACTIVE_DISCOVERY_CONCURRENCY: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchMode { Background, Open }

fn launch_mode(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> LaunchMode {
    if arguments.into_iter().any(|argument| argument == "--open") {
        LaunchMode::Open
    } else {
        LaunchMode::Background
    }
}

fn handle_mobile_command(arguments: &[std::ffi::OsString]) -> bool {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else { return false; };
    if !matches!(command, "--mobile-enable" | "--mobile-disable" | "--mobile-rotate-token" | "--mobile-pairing-url") {
        return false;
    }

    let mut config = config::load();
    match command {
        "--mobile-enable" => {
            config.mobile.enabled = true;
            config.mobile.allow_lan = Some(true);
            config.mobile.bind = "0.0.0.0".into();
            match config::save(&config) {
                Ok(()) => println!(
                    "Mobile access enabled on {}:{}. Restart Agents Usage to apply it.",
                    config.mobile.bind, config.mobile.port
                ),
                Err(error) => eprintln!("Could not enable mobile access: {error}"),
            }
        }
        "--mobile-disable" => {
            config.mobile.enabled = false;
            match config::save(&config) {
                Ok(()) => println!("Mobile access disabled. Restart Agents Usage to apply it."),
                Err(error) => eprintln!("Could not disable mobile access: {error}"),
            }
        }
        "--mobile-rotate-token" => {
            mobile::revoke_all_devices(&mut config);
            match config::save(&config) {
                Ok(()) => println!("All paired phones were revoked. Generate a new pairing link for each phone."),
                Err(error) => eprintln!("Could not revoke paired phones: {error}"),
            }
        }
        "--mobile-pairing-url" => {
            let Some(base_url) = arguments.get(1).and_then(|value| value.to_str()) else {
                eprintln!("Usage: agents-usage --mobile-pairing-url <http-or-https-base-url>");
                return true;
            };
            if !config.mobile.enabled {
                eprintln!("Mobile access is disabled. Run --mobile-enable first.");
                return true;
            }
            let token = mobile::create_pairing(&mut config, 1);
            match mobile_pairing_url(base_url, &token) {
                Ok(url) => match config::save(&config) {
                    Ok(()) => println!("{url}"),
                    Err(error) => eprintln!("Could not save the one-time pairing link: {error}"),
                },
                Err(error) => eprintln!("Could not create pairing URL: {error}"),
            }
        }
        _ => unreachable!(),
    }
    true
}

fn mobile_pairing_url(base_url: &str, token: &str) -> Result<String, &'static str> {
    let mut base = url::Url::parse(base_url.trim())
        .map_err(|_| "the base URL must be a valid HTTP or HTTPS URL")?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err("the base URL must start with http:// or https://");
    }
    if base.host_str().is_none() || !base.username().is_empty() || base.password().is_some() {
        return Err("the base URL must contain a desktop hostname without credentials");
    }
    if base.query().is_some() || base.fragment().is_some() {
        return Err("the base URL must not contain whitespace, a query, or a fragment");
    }
    if token.len() < 32 || !token.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte)) {
        return Err("the mobile pairing token is invalid");
    }
    if !base.path().ends_with('/') {
        base.path_segments_mut()
            .map_err(|_| "the base URL path is invalid")?
            .push("");
    }
    let path = base.path().to_string();
    let mut pairing = base.join("pair").map_err(|_| "the base URL path is invalid")?;
    pairing.query_pairs_mut().append_pair("token", token).append_pair("path", &path);
    Ok(pairing.into())
}

#[derive(Clone, Debug, Default)]
struct MobileEndpoints {
    lan: Option<String>,
    tailscale: Option<String>,
}

fn discover_mobile_endpoints(port: u16) -> MobileEndpoints {
    let route_address = |bind: &str, destination: &str| {
        std::net::UdpSocket::bind(bind)
            .and_then(|socket| {
                socket.connect(destination)?;
                socket.local_addr()
            })
            .ok()
            .map(|address| address.ip())
    };
    let lan = route_address("0.0.0.0:0", "192.0.2.1:80")
        .filter(|address| matches!(address, std::net::IpAddr::V4(value) if value.is_private() || value.is_link_local()))
        .or_else(|| {
            route_address("[::]:0", "[2001:db8::1]:80")
                .filter(|address| matches!(address, std::net::IpAddr::V6(value) if value.is_unique_local()))
        })
        .map(|address| mobile_lan_url(address, port));

    let tailscale_status = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<serde_json::Value>(&output.stdout).ok());
    let tailscale_serve_status = std::process::Command::new("tailscale")
        .args(["serve", "status", "--json"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<serde_json::Value>(&output.stdout).ok());
    let tailscale = tailscale_status.as_ref().and_then(|status| {
        let name = status.get("Self")?.get("DNSName")?.as_str()?.trim_end_matches('.');
        if name.is_empty()
            || status.get("BackendState").and_then(serde_json::Value::as_str) != Some("Running")
            || !tailscale_serve_matches(tailscale_serve_status.as_ref()?, name, port)
        {
            return None;
        }
        Some(format!("https://{name}/agents-usage/"))
    });

    MobileEndpoints { lan, tailscale }
}

fn tailscale_serve_matches(status: &serde_json::Value, dns_name: &str, port: u16) -> bool {
    let expected_proxy = format!("http://127.0.0.1:{port}");
    let expected_site = format!("{}:443", dns_name.trim_end_matches('.'));
    status
        .get("Web")
        .and_then(serde_json::Value::as_object)
        .and_then(|sites| sites.get(&expected_site))
        .and_then(|site| site.get("Handlers"))
        .and_then(|handlers| handlers.get("/agents-usage"))
        .and_then(|handler| handler.get("Proxy"))
        .and_then(serde_json::Value::as_str)
        == Some(expected_proxy.as_str())
}

fn mobile_lan_url(address: std::net::IpAddr, port: u16) -> String {
    match address {
        std::net::IpAddr::V4(address) => format!("http://{address}:{port}/"),
        std::net::IpAddr::V6(address) => format!("http://[{address}]:{port}/"),
    }
}

fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn mobile_pairing_bundle(endpoints: &MobileEndpoints, token: &str) -> Result<String, &'static str> {
    let mut bases = [endpoints.lan.as_deref(), endpoints.tailscale.as_deref()]
        .into_iter()
        .flatten();
    let Some(primary) = bases.next() else {
        return Err("no LAN or Tailscale address could be detected");
    };
    mobile_pairing_url(primary, token)?;
    let mut bundle = format!(
        "agents-usage://pair?token={token}&base={}",
        percent_encode_query(primary)
    );
    if let Some(fallback) = bases.next() {
        mobile_pairing_url(fallback, token)?;
        bundle.push_str("&fallback=");
        bundle.push_str(&percent_encode_query(fallback));
    }
    Ok(bundle)
}

fn qr_cell_model(value: &str) -> ModelRc<QrCell> {
    let Ok(code) = qrcode::QrCode::new(value.as_bytes()) else {
        return ModelRc::default();
    };
    let width = code.width();
    let module_size = (140 / width.max(1)).max(1) as f32;
    let rendered = module_size * width as f32;
    let offset = (156.0 - rendered) / 2.0;
    let mut cells = Vec::new();
    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == qrcode::types::Color::Dark {
                cells.push(QrCell {
                    x: offset + x as f32 * module_size,
                    y: offset + y as f32 * module_size,
                    size: module_size,
                });
            }
        }
    }
    ModelRc::new(VecModel::from(cells))
}

#[derive(Clone, Copy, Debug)]
enum PanelEdge { Top, Bottom, Left, Right }

impl PanelEdge {
    #[cfg(target_os = "linux")]
    fn parse(value: &str) -> Self {
        match value {
            "top" => Self::Top,
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Bottom,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PanelAnchor {
    icon_x: i32,
    icon_y: i32,
    icon_w: i32,
    icon_h: i32,
    monitor_x: i32,
    monitor_y: i32,
    monitor_w: i32,
    monitor_h: i32,
    edge: PanelEdge,
}

impl PanelAnchor {
    #[cfg(target_os = "linux")]
    fn new(values: [i32; 8], edge: &str) -> Self {
        Self {
            icon_x: values[0], icon_y: values[1], icon_w: values[2].max(1), icon_h: values[3].max(1),
            monitor_x: values[4], monitor_y: values[5], monitor_w: values[6].max(1), monitor_h: values[7].max(1),
            edge: PanelEdge::parse(edge),
        }
    }
}

#[derive(Debug)]
enum WorkerCommand {
    #[cfg(target_os = "linux")]
    ToggleAt(PanelAnchor),
    #[cfg(target_os = "linux")]
    OpenAt(PanelAnchor),
    #[cfg(target_os = "linux")]
    OpenSettingsAt(PanelAnchor),
    OpenSettings,
    OpenStandalone,
    ToggleAtPoint { x: i32, y: i32, icon_w: i32, icon_h: i32 },
    Refresh,
    RefreshIfStale,
    RefreshIfStaleMobile,
    PersistSettings,
    MobileConfigChanged,
    MobileDeviceListChanged,
    ConsumeReset { account_id: String, credit_id: String },
    HidePanel,
    #[cfg(target_os = "linux")]
    CheckPopupFocus,
    Tick,
    Quit,
}

struct MobileServerHandle {
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl MobileServerHandle {
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

fn start_mobile_server(
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    refreshing: Arc<AtomicBool>,
    tx: UnboundedSender<WorkerCommand>,
    ui: slint::Weak<MainWindow>,
) -> Option<MobileServerHandle> {
    let mobile_config = config.lock().ok().map(|cfg| cfg.mobile.clone()).unwrap_or_default();
    if !mobile_config.enabled {
        return None;
    }
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<String, String>>();
    let ready_ui = ui.clone();
    tokio::spawn(async move {
        if let Ok(status) = ready_rx.await {
            let message = status.unwrap_or_else(|error| error);
            let _ = ready_ui.upgrade_in_event_loop(move |ui| ui.set_mobile_status(message.into()));
        }
    });
    let task = tokio::spawn(async move {
        if let Err(error) = mobile::serve(
            mobile_config,
            accounts,
            config,
            refreshing,
            tx,
            shutdown_rx,
            ready_tx,
        )
        .await
        {
            eprintln!("mobile: server stopped: {error}");
        }
    });
    Some(MobileServerHandle { shutdown: shutdown_tx, task })
}

fn infer_panel_edge(icon_x: i32, icon_y: i32, icon_w: i32, icon_h: i32, monitor: (i32, i32, i32, i32)) -> PanelEdge {
    let (monitor_x, monitor_y, monitor_w, monitor_h) = monitor;
    let distances = [
        (icon_y - monitor_y).abs(),
        (icon_x - monitor_x).abs(),
        (monitor_x + monitor_w - (icon_x + icon_w)).abs(),
        (monitor_y + monitor_h - (icon_y + icon_h)).abs(),
    ];
    match distances.iter().enumerate().min_by_key(|(_, distance)| *distance).map(|(index, _)| index) {
        Some(0) => PanelEdge::Top,
        Some(1) => PanelEdge::Left,
        Some(2) => PanelEdge::Right,
        _ => PanelEdge::Bottom,
    }
}

fn anchor_for_screen_point(ui: &MainWindow, x: i32, y: i32, icon_w: i32, icon_h: i32) -> Option<PanelAnchor> {
    use slint::winit_030::WinitWindowAccessor;
    let mut anchor = None;
    let _ = ui.window().with_winit_window(|window| {
        let mut monitors = window.available_monitors().collect::<Vec<_>>();
        if monitors.is_empty() {
            if let Some(monitor) = window.current_monitor() { monitors.push(monitor); }
        }
        let center_x = x + icon_w / 2;
        let center_y = y + icon_h / 2;
        let monitor = monitors.iter().find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            center_x >= position.x && center_x < position.x + size.width as i32
                && center_y >= position.y && center_y < position.y + size.height as i32
        }).or_else(|| monitors.first());
        let Some(monitor) = monitor else { return; };
        let position = monitor.position();
        let size = monitor.size();
        let bounds = (position.x, position.y, size.width as i32, size.height as i32);
        anchor = Some(PanelAnchor {
            icon_x: x,
            icon_y: y,
            icon_w: icon_w.max(1),
            icon_h: icon_h.max(1),
            monitor_x: bounds.0,
            monitor_y: bounds.1,
            monitor_w: bounds.2.max(1),
            monitor_h: bounds.3.max(1),
            edge: infer_panel_edge(x, y, icon_w, icon_h, bounds),
        });
    });
    anchor
}

fn send(tx: &UnboundedSender<WorkerCommand>, command: WorkerCommand) { let _ = tx.send(command); }

fn bot_icon_rgba(body: [u8; 4]) -> Vec<u8> {
    const SIZE: usize = 32;
    let mut rgba = vec![0_u8; SIZE * SIZE * 4];
    let mut pixel = |x: usize, y: usize, color: [u8; 4]| {
        let offset = (y * SIZE + x) * 4;
        rgba[offset..offset + 4].copy_from_slice(&color);
    };
    for y in 10..27 {
        for x in 5..27 {
            let rounded_corner = !(8..=23).contains(&x) && !(13..=23).contains(&y);
            if !rounded_corner { pixel(x, y, body); }
        }
    }
    for y in 5..11 { for x in 15..18 { pixel(x, y, body); } }
    for y in 4..7 { for x in 15..23 { pixel(x, y, body); } }
    for y in 16..20 {
        for x in 10..13 { pixel(x, y, [255, 255, 255, 255]); }
        for x in 20..23 { pixel(x, y, [255, 255, 255, 255]); }
    }
    rgba
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct GnomeBridge {
    tx: UnboundedSender<WorkerCommand>,
    visible: Arc<AtomicBool>,
}

#[cfg(target_os = "linux")]
#[zbus::interface(name = "io.github.agentsusagetray.GnomeBridge1")]
#[allow(clippy::too_many_arguments)]
impl GnomeBridge {
    fn open(&self) -> bool {
        send(&self.tx, WorkerCommand::OpenStandalone);
        true
    }

    fn toggle_at(
        &self,
        icon_x: i32, icon_y: i32, icon_w: i32, icon_h: i32,
        monitor_x: i32, monitor_y: i32, monitor_w: i32, monitor_h: i32,
        edge: &str,
    ) -> bool {
        let opening = !self.visible.load(Ordering::SeqCst);
        send(&self.tx, WorkerCommand::ToggleAt(PanelAnchor::new(
            [icon_x, icon_y, icon_w, icon_h, monitor_x, monitor_y, monitor_w, monitor_h], edge,
        )));
        opening
    }

    fn open_at(
        &self,
        icon_x: i32, icon_y: i32, icon_w: i32, icon_h: i32,
        monitor_x: i32, monitor_y: i32, monitor_w: i32, monitor_h: i32,
        edge: &str,
    ) -> bool {
        send(&self.tx, WorkerCommand::OpenAt(PanelAnchor::new(
            [icon_x, icon_y, icon_w, icon_h, monitor_x, monitor_y, monitor_w, monitor_h], edge,
        )));
        true
    }

    fn open_settings_at(
        &self,
        icon_x: i32, icon_y: i32, icon_w: i32, icon_h: i32,
        monitor_x: i32, monitor_y: i32, monitor_w: i32, monitor_h: i32,
        edge: &str,
    ) -> bool {
        send(&self.tx, WorkerCommand::OpenSettingsAt(PanelAnchor::new(
            [icon_x, icon_y, icon_w, icon_h, monitor_x, monitor_y, monitor_w, monitor_h], edge,
        )));
        true
    }

    fn refresh(&self) { send(&self.tx, WorkerCommand::Refresh); }
    fn quit(&self) { send(&self.tx, WorkerCommand::Quit); }
    fn is_visible(&self) -> bool { self.visible.load(Ordering::SeqCst) }
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct StatusNotifierTray {
    tx: UnboundedSender<WorkerCommand>,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for StatusNotifierTray {
    fn id(&self) -> String { "agents-usage".into() }
    fn title(&self) -> String { "Agents Usage".into() }
    fn icon_name(&self) -> String { "agents-usage".into() }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let mut data = bot_icon_rgba([39, 191, 206, 255]);
        for pixel in data.chunks_exact_mut(4) { pixel.rotate_right(1); }
        vec![ksni::Icon { width: 32, height: 32, data }]
    }

    fn activate(&mut self, x: i32, y: i32) {
        if x == 0 && y == 0 {
            send(&self.tx, WorkerCommand::OpenStandalone);
        } else {
            send(&self.tx, WorkerCommand::ToggleAtPoint {
                x: x - 12,
                y: y - 12,
                icon_w: 24,
                icon_h: 24,
            });
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open".into(),
                activate: Box::new(|tray: &mut Self| send(&tray.tx, WorkerCommand::OpenStandalone)),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Settings".into(),
                activate: Box::new(|tray: &mut Self| send(&tray.tx, WorkerCommand::OpenSettings)),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Refresh".into(),
                activate: Box::new(|tray: &mut Self| send(&tray.tx, WorkerCommand::Refresh)),
                ..Default::default()
            }.into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| send(&tray.tx, WorkerCommand::Quit)),
                ..Default::default()
            }.into(),
        ]
    }
}

#[cfg(target_os = "linux")]
fn should_use_status_notifier() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    desktop_uses_status_notifier(&desktop)
}

#[cfg(any(target_os = "linux", test))]
fn desktop_uses_status_notifier(desktop: &str) -> bool {
    !desktop.to_ascii_lowercase().contains("gnome")
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn native_tray_icon() -> Result<tray_icon::Icon, tray_icon::BadIcon> {
    let body = if cfg!(target_os = "macos") { [0, 0, 0, 255] } else { [39, 191, 206, 255] };
    tray_icon::Icon::from_rgba(bot_icon_rgba(body), 32, 32)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn create_native_tray(tx: UnboundedSender<WorkerCommand>) -> Result<tray_icon::TrayIcon, String> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = Menu::new();
    let open = MenuItem::with_id("open", "Open", true, None);
    let settings = MenuItem::with_id("settings", "Settings", true, None);
    let refresh = MenuItem::with_id("refresh", "Refresh", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&open, &settings, &refresh, &quit]).map_err(|error| error.to_string())?;

    let click_tx = tx.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            rect,
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event {
            send(&click_tx, WorkerCommand::ToggleAtPoint {
                x: rect.position.x.round() as i32,
                y: rect.position.y.round() as i32,
                icon_w: rect.size.width as i32,
                icon_h: rect.size.height as i32,
            });
        }
    }));
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.as_ref() {
        "open" => send(&tx, WorkerCommand::OpenStandalone),
        "settings" => send(&tx, WorkerCommand::OpenSettings),
        "refresh" => send(&tx, WorkerCommand::Refresh),
        "quit" => send(&tx, WorkerCommand::Quit),
        _ => {}
    }));

    TrayIconBuilder::new()
        .with_tooltip("Agents Usage")
        .with_icon(native_tray_icon().map_err(|error| error.to_string())?)
        .with_icon_as_template(cfg!(target_os = "macos"))
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(true)
        .build()
        .map_err(|error| error.to_string())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn acquire_native_instance_lock() -> std::io::Result<Option<std::fs::File>> {
    use std::fs::OpenOptions;
    let directory = config::app_config_dir();
    std::fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("instance.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn x11_window_id(window: &slint::winit_030::winit::window::Window) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        RawWindowHandle::Xlib(handle) if handle.window <= u32::MAX as _ => Some(handle.window as u32),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn intern_atom<C: x11rb::connection::Connection>(connection: &C, name: &[u8]) -> Result<u32, Box<dyn std::error::Error>> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    Ok(connection.intern_atom(false, name)?.reply()?.atom)
}

#[cfg(target_os = "linux")]
fn prepare_x11_popup_native(xid: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;
    let (connection, _) = x11rb::connect(None)?;
    let window_type = intern_atom(&connection, b"_NET_WM_WINDOW_TYPE")?;
    let popup = intern_atom(&connection, b"_NET_WM_WINDOW_TYPE_POPUP_MENU")?;
    let state = intern_atom(&connection, b"_NET_WM_STATE")?;
    let skip_taskbar = intern_atom(&connection, b"_NET_WM_STATE_SKIP_TASKBAR")?;
    let skip_pager = intern_atom(&connection, b"_NET_WM_STATE_SKIP_PAGER")?;
    let above = intern_atom(&connection, b"_NET_WM_STATE_ABOVE")?;
    connection.change_window_attributes(xid, &ChangeWindowAttributesAux::new().override_redirect(1u32))?.check()?;
    connection.change_property32(PropMode::REPLACE, xid, window_type, AtomEnum::ATOM, &[popup])?.check()?;
    connection.change_property32(PropMode::REPLACE, xid, state, AtomEnum::ATOM, &[skip_taskbar, skip_pager, above])?.check()?;
    connection.flush()?;
    let attrs = connection.get_window_attributes(xid)?.reply()?;
    eprintln!("popup prewarm: xid=0x{xid:x} override_redirect={} map_state={:?}", attrs.override_redirect, attrs.map_state);
    Ok(())
}

#[cfg(target_os = "linux")]
fn configure_x11_popup_geometry(xid: u32, x: i32, y: i32, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, StackMode};
    let (connection, _) = x11rb::connect(None)?;
    connection.configure_window(xid, &ConfigureWindowAux::new().x(x).y(y).width(width).height(height).stack_mode(StackMode::ABOVE))?.check()?;
    connection.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn focus_x11_popup(xid: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, InputFocus};
    let (connection, _) = x11rb::connect(None)?;
    connection.set_input_focus(InputFocus::PARENT, xid, x11rb::CURRENT_TIME)?.check()?;
    connection.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn x11_popup_has_input_focus(xid: u32) -> bool {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let Ok((connection, _)) = x11rb::connect(None) else { return false; };
    connection
        .get_input_focus()
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.focus == xid)
}

fn panel_position_for_size(anchor: PanelAnchor, panel_w: i32, panel_h: i32) -> slint::PhysicalPosition {
    let panel_w = panel_w.max(1);
    let panel_h = panel_h.max(1);
    let icon_center_x = anchor.icon_x + anchor.icon_w / 2;
    let icon_center_y = anchor.icon_y + anchor.icon_h / 2;
    let (mut x, mut y) = match anchor.edge {
        PanelEdge::Top => (icon_center_x - panel_w / 2, anchor.icon_y + anchor.icon_h + PANEL_GAP_PX),
        PanelEdge::Left => (anchor.icon_x + anchor.icon_w + PANEL_GAP_PX, icon_center_y - panel_h / 2),
        PanelEdge::Right => (anchor.icon_x - panel_w - PANEL_GAP_PX, icon_center_y - panel_h / 2),
        PanelEdge::Bottom => (icon_center_x - panel_w / 2, anchor.icon_y - panel_h - PANEL_GAP_PX),
    };
    let min_x = anchor.monitor_x + SCREEN_MARGIN_PX;
    let min_y = anchor.monitor_y + SCREEN_MARGIN_PX;
    let max_x = anchor.monitor_x + anchor.monitor_w - panel_w - SCREEN_MARGIN_PX;
    let max_y = anchor.monitor_y + anchor.monitor_h - panel_h - SCREEN_MARGIN_PX;
    x = x.clamp(min_x, max_x.max(min_x));
    y = y.clamp(min_y, max_y.max(min_y));
    slint::PhysicalPosition::new(x, y)
}

fn position_panel_for_physical_size(ui: &MainWindow, anchor: PanelAnchor, width: u32, height: u32) {
    use slint::winit_030::WinitWindowAccessor;
    let position = panel_position_for_size(anchor, width as i32, height as i32);
    let _ = ui.window().with_winit_window(|window| {
        window.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(position.x, position.y));
        let _ = window.request_inner_size(slint::winit_030::winit::dpi::PhysicalSize::new(width, height));
        #[cfg(target_os = "linux")]
        if let Some(xid) = x11_window_id(window)
            && let Err(error) = configure_x11_popup_geometry(xid, position.x, position.y, width, height)
        { eprintln!("popup placement failed: {error}"); }
    });
}

fn position_panel_for_desired_height(ui: &MainWindow, anchor: PanelAnchor, logical_height: f32) {
    use slint::winit_030::WinitWindowAccessor;
    let mut target = None;
    let _ = ui.window().with_winit_window(|window| {
        let scale = window.scale_factor();
        let width = window.inner_size().width.max(1);
        let height = (f64::from(logical_height.max(1.0)) * scale).round().max(1.0) as u32;
        target = Some((width, height));
    });
    if let Some((width, height)) = target { position_panel_for_physical_size(ui, anchor, width, height); }
}

fn position_panel_standalone(ui: &MainWindow, logical_height: f32) {
    use slint::winit_030::WinitWindowAccessor;
    let _ = ui.window().with_winit_window(|window| {
        let scale = window.scale_factor();
        let width = window.inner_size().width.max(1);
        let height = (f64::from(logical_height.max(1.0)) * scale).round().max(1.0) as u32;
        let monitor = window.current_monitor().or_else(|| window.available_monitors().next());
        let Some(monitor) = monitor else { return; };
        let monitor_position = monitor.position();
        let monitor_size = monitor.size();
        let width = width.min(monitor_size.width.saturating_sub((SCREEN_MARGIN_PX * 2) as u32).max(1));
        let height = height.min(monitor_size.height.saturating_sub((SCREEN_MARGIN_PX * 2) as u32).max(1));
        let x = monitor_position.x + (monitor_size.width.saturating_sub(width) / 2) as i32;
        let y = monitor_position.y + (monitor_size.height.saturating_sub(height) / 2) as i32;
        window.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(x, y));
        let _ = window.request_inner_size(slint::winit_030::winit::dpi::PhysicalSize::new(width, height));
        #[cfg(target_os = "linux")]
        if let Some(xid) = x11_window_id(window)
            && let Err(error) = configure_x11_popup_geometry(xid, x, y, width, height)
        { eprintln!("standalone popup placement failed: {error}"); }
    });
}

fn activate_visible_popup(ui: &MainWindow, native_xid_shared: &Arc<Mutex<Option<u32>>>) {
    #[cfg(not(target_os = "linux"))]
    let _ = native_xid_shared;
    use slint::winit_030::WinitWindowAccessor;
    let _ = ui.window().with_winit_window(|window| {
        window.request_redraw();
        #[cfg(target_os = "linux")]
        if let Some(xid) = x11_window_id(window) {
            let needs_preparation = native_xid_shared
                .lock()
                .map(|guard| *guard != Some(xid))
                .unwrap_or(true);
            if needs_preparation {
                match prepare_x11_popup_native(xid) {
                    Ok(()) => {
                        if let Ok(mut guard) = native_xid_shared.lock() { *guard = Some(xid); }
                    }
                    Err(error) => eprintln!("X11 popup preparation failed: {error}"),
                }
            }
            let _ = focus_x11_popup(xid);
        }
        #[cfg(not(target_os = "linux"))]
        window.focus_window();
    });
}

fn show_dashboard(ui: &MainWindow, anchor: Option<PanelAnchor>, native_xid_shared: &Arc<Mutex<Option<u32>>>) {
    let started = Instant::now();
    ui.set_settings_visible(false);
    ui.set_desired_height_px(ui.get_dashboard_height_px());
    if let Some(anchor) = anchor {
        position_panel_for_desired_height(ui, anchor, ui.get_desired_height_px());
    } else {
        position_panel_standalone(ui, ui.get_desired_height_px());
    }
    let _ = ui.show();
    if let Some(anchor) = anchor {
        position_panel_for_desired_height(ui, anchor, ui.get_desired_height_px());
    } else {
        position_panel_standalone(ui, ui.get_desired_height_px());
    }
    activate_visible_popup(ui, native_xid_shared);
    eprintln!("dashboard opened in {:?}", started.elapsed());
}

fn show_settings(ui: &MainWindow, anchor: Option<PanelAnchor>, native_xid_shared: &Arc<Mutex<Option<u32>>>) {
    let settings_height = ui.get_desired_height_px().max(1.0);
    ui.set_settings_height_px(settings_height);
    ui.set_settings_visible(true);
    ui.set_desired_height_px(settings_height);
    if let Some(anchor) = anchor {
        position_panel_for_desired_height(ui, anchor, ui.get_desired_height_px());
    } else {
        position_panel_standalone(ui, ui.get_desired_height_px());
    }
    let _ = ui.show();
    if let Some(anchor) = anchor {
        position_panel_for_desired_height(ui, anchor, ui.get_desired_height_px());
    } else {
        position_panel_standalone(ui, ui.get_desired_height_px());
    }
    activate_visible_popup(ui, native_xid_shared);
}

fn schedule_show_dashboard(
    ui_weak: slint::Weak<MainWindow>,
    anchor: Option<PanelAnchor>,
    native_xid: Arc<Mutex<Option<u32>>>,
    panel_visible: Arc<AtomicBool>,
) {
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_weak.upgrade() else { return; };
        let _ = slint::spawn_local(async move {
            use slint::winit_030::WinitWindowAccessor as _;
            let _ = ui.window().winit_window().await;
            if !panel_visible.load(Ordering::SeqCst) { return; }
            show_dashboard(&ui, anchor, &native_xid);
        });
    });
}

fn schedule_show_settings(
    ui_weak: slint::Weak<MainWindow>,
    anchor: Option<PanelAnchor>,
    native_xid: Arc<Mutex<Option<u32>>>,
    panel_visible: Arc<AtomicBool>,
) {
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_weak.upgrade() else { return; };
        let _ = slint::spawn_local(async move {
            use slint::winit_030::WinitWindowAccessor as _;
            let _ = ui.window().winit_window().await;
            if !panel_visible.load(Ordering::SeqCst) { return; }
            show_settings(&ui, anchor, &native_xid);
        });
    });
}

fn render_ui(
    ui: &MainWindow,
    accounts: &Arc<Mutex<Vec<AccountRecord>>>,
    config: &Arc<Mutex<AppConfig>>,
    last_anchor: &Arc<Mutex<Option<PanelAnchor>>>,
    empty_text: Option<&str>,
) {
    let records = accounts.lock().map(|value| value.clone()).unwrap_or_default();
    let cfg = config.lock().map(|value| value.clone()).unwrap_or_default();
    let (model, enabled_count) = ui_model::model(&records, cfg.show_banked_resets);
    let dashboard_height = ui_model::panel_height(&records, cfg.show_banked_resets);
    let height = if ui.get_settings_visible() {
        ui.get_settings_height_px()
    } else {
        dashboard_height
    };
    ui.set_dashboard_height_px(dashboard_height);
    ui.set_accounts(model);
    ui.set_enabled_account_count(enabled_count as i32);
    ui.set_blur_emails(cfg.blur_emails);
    ui.set_blur_names(cfg.blur_names);
    ui.set_color_reset_timers(cfg.color_reset_timers);
    ui.set_usage_bar_color_mode(cfg.usage_bar_color_mode.as_str().into());
    ui.set_usage_bar_custom_color(ui_model::color_from_name(&cfg.usage_bar_custom_color));
    ui.set_always_show_reset_counter(cfg.always_show_reset_counter);
    ui.set_show_banked_resets(cfg.show_banked_resets);
    ui.set_mobile_enabled(cfg.mobile.enabled);
    ui.set_mobile_allow_lan(cfg.mobile.allows_lan());
    if !cfg.mobile.enabled {
        ui.set_mobile_status("Disabled".into());
    } else if ui.get_mobile_status() == "Disabled" {
        ui.set_mobile_status(format!("Starting on port {}…", cfg.mobile.port).into());
    }
    let now = chrono::Utc::now().timestamp();
    let mobile_devices = cfg
        .mobile
        .devices
        .iter()
        .filter(|device| mobile::device_is_active(device, now))
        .map(|device| MobileDeviceView {
            id: device.id.clone().into(),
            name: device.name.clone().into(),
            detail: match device.last_seen_at {
                Some(last_seen) if now - last_seen < 120 => "Seen recently".into(),
                Some(last_seen) => format!("Last seen {}h ago", ((now - last_seen) / 3600).max(1)).into(),
                None => "Paired before this update".into(),
            },
        })
        .collect::<Vec<_>>();
    ui.set_mobile_devices(ModelRc::new(VecModel::from(mobile_devices)));
    let pairing_active = cfg.mobile.pairing.as_ref().is_some_and(|pairing| {
        pairing.expires_at >= now && pairing.remaining_uses > 0
    });
    if !pairing_active {
        ui.set_mobile_pairing_link("".into());
        ui.set_mobile_qr_cells(ModelRc::default());
    }
    ui.set_accounts_summary(format!("{} discovered · refresh also checks for new provider accounts", records.len()).into());
    if let Some(text) = empty_text { ui.set_empty_text(text.into()); }
    ui.set_desired_height_px(height);
    if let Some(anchor) = last_anchor.lock().ok().and_then(|value| *value) {
        position_panel_for_desired_height(ui, anchor, height);
    }
}

fn schedule_render(
    ui: slint::Weak<MainWindow>,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    last_anchor: Arc<Mutex<Option<PanelAnchor>>>,
    empty_text: Option<String>,
) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        render_ui(&ui, &accounts, &config, &last_anchor, empty_text.as_deref());
    });
}

fn canonical_id(path: &Path) -> String {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

fn color_index_for_home(path: &Path) -> usize {
    if path.file_name().is_some_and(|name| name == ".codex") {
        return 0;
    }
    // Stable FNV-1a selection for arbitrary roots. This is presentation metadata only.
    let mut hash: u64 = 0xcbf29ce484222325;
    let canonical = canonical_id(path);
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as usize
}

fn normalized_account_color(value: Option<&str>, fallback: &str) -> String {
    match value {
        Some("white") => "gray".into(),
        Some(value) if ui_model::is_account_color(value) => value.into(),
        _ => fallback.into(),
    }
}

fn source_account_name(provider_id: &str, path: &Path) -> String {
    if provider_id != providers::OPENAI {
        return providers::display_name(provider_id).into();
    }
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("OpenAI Codex")
        .to_string()
}

fn account_id(provider_id: &str, home: &Path) -> String {
    format!("{provider_id}:{}", canonical_id(home))
}

fn default_account_name(record: &AccountRecord) -> String {
    record
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.email.as_deref())
        .and_then(|email| email.split('@').next())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| source_account_name(&record.provider_id, &record.home))
}

fn normalized_display_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.chars().take(64).collect())
    }
}

fn normalized_account_email(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn reconcile_preference_for_snapshot(
    config: &mut AppConfig,
    provider_id: &str,
    home: &Path,
    email: Option<&str>,
) {
    let identity = normalized_account_email(email);
    let path_index = config
        .accounts
        .iter()
        .position(|pref| pref.provider_id == provider_id && canonical_id(&pref.home) == canonical_id(home));
    let identity_indices = identity
        .as_deref()
        .map(|identity| {
            config
                .accounts
                .iter()
                .enumerate()
                .filter_map(|(index, pref)| {
                    (pref.provider_id == provider_id
                        && normalized_account_email(pref.identity_email.as_deref()).as_deref() == Some(identity))
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let chosen_index = identity_indices.first().copied().or(path_index);
    let Some(chosen_index) = chosen_index else {
        config.accounts.push(AccountPreference {
            provider_id: provider_id.into(),
            home: home.to_path_buf(),
            identity_email: identity,
            ..AccountPreference::default()
        });
        return;
    };

    let mut chosen = config.accounts[chosen_index].clone();
    chosen.provider_id = provider_id.into();
    chosen.home = home.to_path_buf();
    chosen.identity_email = identity;
    let mut remove = identity_indices.into_iter().collect::<HashSet<_>>();
    if let Some(path_index) = path_index { remove.insert(path_index); }
    remove.remove(&chosen_index);
    config.accounts = config
        .accounts
        .drain(..)
        .enumerate()
        .filter_map(|(index, pref)| {
            if index == chosen_index {
                Some(chosen.clone())
            } else if remove.contains(&index) {
                None
            } else {
                Some(pref)
            }
        })
        .collect();
}

fn reconcile_cached_accounts(
    config: &mut AppConfig,
    cache: Vec<CachedUsage>,
) -> (Vec<CachedUsage>, bool) {
    let original_config = config.clone();

    for pref in &mut config.accounts {
        let cached_email = cache
            .iter()
            .find(|cached| cached.provider_id == pref.provider_id && canonical_id(&cached.home) == canonical_id(&pref.home))
            .and_then(|cached| normalized_account_email(cached.snapshot.email.as_deref()));
        pref.identity_email = cached_email
            .or_else(|| normalized_account_email(pref.identity_email.as_deref()));
    }

    let identities = config
        .accounts
        .iter()
        .filter_map(|pref| normalized_account_email(pref.identity_email.as_deref()).map(|email| (pref.provider_id.clone(), email)))
        .collect::<Vec<_>>();
    let mut reconciled = HashSet::new();
    for (provider_id, identity) in identities {
        if !reconciled.insert((provider_id.clone(), identity.clone())) { continue; }
        let matching = config
            .accounts
            .iter()
            .enumerate()
            .filter_map(|(index, pref)| {
                (pref.provider_id == provider_id
                    && normalized_account_email(pref.identity_email.as_deref()).as_deref() == Some(identity.as_str()))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if matching.len() < 2 { continue; }

        // Discovery appends a newly seen path. Keep the original account's
        // preferences and ordering, but point it at the newest path that still exists.
        let current_home = matching
            .iter()
            .rev()
            .find_map(|index| providers::is_marked(&provider_id, &config.accounts[*index].home).then(|| config.accounts[*index].home.clone()))
            .unwrap_or_else(|| config.accounts[*matching.last().expect("duplicate group is non-empty")].home.clone());
        let chosen_index = matching[0];
        let mut chosen = config.accounts[chosen_index].clone();
        chosen.home = current_home;
        chosen.identity_email = Some(identity);
        let remove = matching.into_iter().skip(1).collect::<HashSet<_>>();
        config.accounts = config
            .accounts
            .drain(..)
            .enumerate()
            .filter_map(|(index, pref)| {
                if index == chosen_index {
                    Some(chosen.clone())
                } else if remove.contains(&index) {
                    None
                } else {
                    Some(pref)
                }
            })
            .collect();
    }

    let reconciled_cache = config
        .accounts
        .iter()
        .filter_map(|pref| {
            cache
                .iter()
                .rev()
                .find(|cached| cached.provider_id == pref.provider_id && canonical_id(&cached.home) == canonical_id(&pref.home))
                .or_else(|| {
                    let identity = normalized_account_email(pref.identity_email.as_deref())?;
                    cache.iter().rev().find(|cached| {
                        cached.provider_id == pref.provider_id
                            && normalized_account_email(cached.snapshot.email.as_deref()).as_deref()
                            == Some(identity.as_str())
                    })
                })
                .map(|cached| CachedUsage {
                    provider_id: pref.provider_id.clone(),
                    home: pref.home.clone(),
                    snapshot: cached.snapshot.clone(),
                })
        })
        .collect::<Vec<_>>();
    let changed = *config != original_config
        || reconciled_cache.len() != cache.len()
        || reconciled_cache.iter().zip(&cache).any(|(left, right)| {
            left.provider_id != right.provider_id
                || canonical_id(&left.home) != canonical_id(&right.home)
                || normalized_account_email(left.snapshot.email.as_deref())
                    != normalized_account_email(right.snapshot.email.as_deref())
        });
    (reconciled_cache, changed)
}

fn color_hex(color: slint::Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue())
}

fn move_target(index: usize, len: usize, direction: i32) -> Option<usize> {
    if direction == 0 { return None; }
    let target = index as isize + direction.signum() as isize;
    (target >= 0 && (target as usize) < len).then_some(target as usize)
}

fn move_account(
    records: &mut [AccountRecord],
    config: &mut AppConfig,
    id: &str,
    direction: i32,
) -> bool {
    let Some(index) = records.iter().position(|record| record.id == id) else { return false; };
    let Some(target) = move_target(index, records.len(), direction) else { return false; };
    let home = records[index].home.clone();
    let target_home = records[target].home.clone();
    let provider_id = records[index].provider_id.clone();
    let target_provider_id = records[target].provider_id.clone();
    let pref_index = config.accounts.iter().position(|pref| {
        pref.provider_id == provider_id && canonical_id(&pref.home) == canonical_id(&home)
    });
    let target_pref_index = config.accounts.iter().position(|pref| {
        pref.provider_id == target_provider_id && canonical_id(&pref.home) == canonical_id(&target_home)
    });
    let (Some(pref_index), Some(target_pref_index)) = (pref_index, target_pref_index) else { return false; };
    records.swap(index, target);
    config.accounts.swap(pref_index, target_pref_index);
    true
}

fn record_from_snapshot(
    provider_id: &str,
    home: PathBuf,
    snapshot: crate::domain::UsageSnapshot,
    notice: Option<String>,
    config: &mut AppConfig,
    color_index: usize,
) -> AccountRecord {
    let id = account_id(provider_id, &home);
    reconcile_preference_for_snapshot(config, provider_id, &home, snapshot.email.as_deref());
    let discovered_name = snapshot
        .email
        .as_deref()
        .and_then(|email| email.split('@').next())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| source_account_name(provider_id, &home));
    let pref = config::preference_for_provider_mut(config, provider_id, &home);
    let fallback_color = ui_model::ACCOUNT_COLORS[color_index % ui_model::ACCOUNT_COLORS.len()];
    let color_name = normalized_account_color(pref.color.as_deref(), fallback_color);
    if pref.color.as_deref() != Some(color_name.as_str()) { pref.color = Some(color_name.clone()); }
    if pref.display_name.is_none() { pref.display_name = Some(discovered_name.clone()); }
    AccountRecord {
        id,
        home,
        provider_id: provider_id.into(),
        display_name: pref.display_name.clone().unwrap_or(discovered_name),
        color_name,
        enabled: pref.enabled,
        pin_short: pref.pin_short,
        expanded: pref.expanded,
        name_revealed: false,
        email_revealed: false,
        confirm_credit_id: String::new(),
        snapshot: Some(snapshot),
        last_error: notice,
    }
}

fn placeholder_record(
    pref: &AccountPreference,
    index: usize,
    cached_snapshot: Option<UsageSnapshot>,
) -> Option<AccountRecord> {
    if !providers::is_marked(&pref.provider_id, &pref.home) { return None; }
    Some(AccountRecord {
        id: account_id(&pref.provider_id, &pref.home),
        home: pref.home.clone(),
        provider_id: pref.provider_id.clone(),
        display_name: pref.display_name.clone().unwrap_or_else(|| source_account_name(&pref.provider_id, &pref.home)),
        color_name: normalized_account_color(
            pref.color.as_deref(),
            ui_model::ACCOUNT_COLORS[index % ui_model::ACCOUNT_COLORS.len()],
        ),
        enabled: pref.enabled,
        pin_short: pref.pin_short,
        expanded: pref.expanded,
        name_revealed: false,
        email_revealed: false,
        confirm_credit_id: String::new(),
        snapshot: cached_snapshot,
        last_error: None,
    })
}

fn persist_usage_cache(accounts: &Arc<Mutex<Vec<AccountRecord>>>) {
    let cache = accounts
        .lock()
        .map(|records| {
            records
                .iter()
                .filter_map(|record| {
                    record.snapshot.clone().map(|snapshot| CachedUsage {
                        provider_id: record.provider_id.clone(),
                        home: record.home.clone(),
                        snapshot,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Err(error) = config::save_usage_cache(&cache) {
        eprintln!("cache: could not persist usage snapshots: {error}");
    }
}

fn apply_snapshot(
    provider_id: String,
    home: PathBuf,
    snapshot: crate::domain::UsageSnapshot,
    notice: Option<String>,
    accounts: &Arc<Mutex<Vec<AccountRecord>>>,
    config: &Arc<Mutex<AppConfig>>,
) {
    let id = account_id(&provider_id, &home);
    let identity = normalized_account_email(snapshot.email.as_deref());
    let color_index = color_index_for_home(&home);
    let mut new_record = {
        let mut cfg = match config.lock() {
            Ok(cfg) => cfg,
            Err(_) => return,
        };
        let previous = cfg.clone();
        let record = record_from_snapshot(&provider_id, home, snapshot, notice, &mut cfg, color_index);
        if *cfg != previous {
            if let Err(error) = config::save(&cfg) {
                eprintln!("settings: could not persist account discovery: {error}");
            }
        }
        record
    };

    if let Ok(mut records) = accounts.lock() {
        let existing_index = records.iter().position(|record| {
            record.provider_id == provider_id && (record.id == id
                || identity.as_deref().is_some_and(|identity| {
                    normalized_account_email(record.snapshot.as_ref().and_then(|snapshot| snapshot.email.as_deref()))
                        .as_deref()
                        == Some(identity)
                }))
        });
        if let Some(index) = existing_index {
            let existing = records.remove(index);
            new_record.expanded = existing.expanded;
            new_record.name_revealed = existing.name_revealed;
            new_record.email_revealed = existing.email_revealed;
            new_record.confirm_credit_id = existing.confirm_credit_id;
        }
        records.retain(|record| {
            record.provider_id != provider_id || (record.id != id
                && !identity.as_deref().is_some_and(|identity| {
                    normalized_account_email(record.snapshot.as_ref().and_then(|snapshot| snapshot.email.as_deref()))
                        .as_deref()
                        == Some(identity)
                }))
        });
        records.push(new_record);
        let order = config
            .lock()
            .map(|cfg| {
                cfg.accounts
                    .iter()
                    .enumerate()
                    .map(|(index, pref)| (account_id(&pref.provider_id, &pref.home), index))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        records.sort_by_key(|record| order.get(&record.id).copied().unwrap_or(usize::MAX));
    }
    persist_usage_cache(accounts);
}

fn apply_refresh_error(
    id: &str,
    message: String,
    accounts: &Arc<Mutex<Vec<AccountRecord>>>,
) {
    if let Ok(mut records) = accounts.lock()
        && let Some(record) = records.iter_mut().find(|record| record.id == id)
    {
        record.last_error = Some(message);
    }
}

async fn refresh_known_accounts(
    codex_path: Option<PathBuf>,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    ui: slint::Weak<MainWindow>,
    last_anchor: Arc<Mutex<Option<PanelAnchor>>>,
    rpc_semaphore: Arc<Semaphore>,
    operation_concurrency: usize,
) {
    let targets = accounts
        .lock()
        .map(|records| {
            let mut seen = HashSet::new();
            records
                .iter()
                .filter(|record| record.enabled)
                .filter_map(|record| {
                    let identity = normalized_account_email(
                        record.snapshot.as_ref().and_then(|snapshot| snapshot.email.as_deref()),
                    )
                    .unwrap_or_else(|| record.id.clone());
                    let dedupe_key = (record.provider_id.clone(), identity);
                    seen.insert(dedupe_key).then(|| (
                        record.id.clone(),
                        providers::ProviderCandidate {
                            provider_id: record.provider_id.clone(),
                            home: record.home.clone(),
                        },
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if targets.is_empty() {
        return;
    }

    let mut jobs = JoinSet::new();
    let operation_semaphore = Arc::new(Semaphore::new(operation_concurrency.max(1)));
    for (id, candidate) in targets {
        let codex_path = codex_path.clone();
        let rpc_semaphore = rpc_semaphore.clone();
        let operation_semaphore = operation_semaphore.clone();
        jobs.spawn(async move {
            let Ok(_operation_permit) = operation_semaphore.acquire_owned().await else { return None; };
            let Ok(_permit) = rpc_semaphore.acquire_owned().await else { return None; };
            let result = providers::read_account(&candidate, codex_path.as_deref()).await;
            Some((id, candidate, result))
        });
    }

    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(Some((_id, candidate, Ok(reading)))) => {
                apply_snapshot(candidate.provider_id, candidate.home, reading.snapshot, reading.notice, &accounts, &config);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
            Ok(Some((id, _candidate, Err(error)))) => {
                eprintln!("refresh: {id}: {error}");
                let user_message = error.user_message();
                apply_refresh_error(&id, user_message, &accounts);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
            Ok(None) => {}
            Err(error) => eprintln!("refresh task failed: {error}"),
        }
    }
}

async fn discover_new_accounts(
    codex_path: Option<PathBuf>,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    ui: slint::Weak<MainWindow>,
    last_anchor: Arc<Mutex<Option<PanelAnchor>>>,
    rpc_semaphore: Arc<Semaphore>,
    operation_concurrency: usize,
) {
    let cfg = config.lock().map(|cfg| cfg.clone()).unwrap_or_default();
    let candidates = providers::candidates(&cfg);
    let known = accounts
        .lock()
        .map(|records| {
            records
                .iter()
                .map(|record| (record.provider_id.clone(), canonical_id(&record.home)))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let unknown = candidates
        .into_iter()
        .filter(|candidate| !known.contains(&(candidate.provider_id.clone(), canonical_id(&candidate.home))))
        .collect::<Vec<_>>();

    if unknown.is_empty() {
        return;
    }

    eprintln!("discovery: probing {} new provider account candidate(s)", unknown.len());
    let cached_by_identity = config::load_usage_cache()
        .into_iter()
        .filter_map(|cached| {
            normalized_account_email(cached.snapshot.email.as_deref())
                .map(|identity| ((cached.provider_id, identity), cached.snapshot))
        })
        .collect::<HashMap<_, _>>();
    let mut jobs = JoinSet::new();
    let operation_semaphore = Arc::new(Semaphore::new(operation_concurrency.max(1)));
    for (order, candidate) in unknown.into_iter().enumerate() {
        let codex_path = codex_path.clone();
        let rpc_semaphore = rpc_semaphore.clone();
        let operation_semaphore = operation_semaphore.clone();
        jobs.spawn(async move {
            let Ok(_operation_permit) = operation_semaphore.acquire_owned().await else { return None; };
            let Ok(_permit) = rpc_semaphore.acquire_owned().await else { return None; };
            let result = providers::read_account(&candidate, codex_path.as_deref()).await;
            let identity_email = match &result {
                Ok(reading) => reading.snapshot.email.clone(),
                Err(_) if candidate.provider_id == providers::OPENAI => match codex_path.as_deref() {
                    Some(codex_path) => codex::read_openai_identity(codex_path, &candidate.home).await.ok().flatten(),
                    None => None,
                },
                Err(_) => None,
            };
            Some((order, candidate, result, identity_email))
        });
    }

    let mut completed = Vec::new();
    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(Some(value)) => completed.push(value),
            Ok(None) => {}
            Err(error) => eprintln!("discovery task failed: {error}"),
        }
    }
    completed.sort_by_key(|(order, _, _, _)| *order);
    for (_, candidate, result, identity_email) in completed {
        match result {
            Ok(reading) => {
                eprintln!("discovery: found {} account at {}", providers::display_name(&candidate.provider_id), candidate.home.display());
                apply_snapshot(candidate.provider_id, candidate.home, reading.snapshot, reading.notice, &accounts, &config);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
            Err(error) => {
                let user_message = error.user_message().to_string();
                let cached_snapshot = identity_email
                    .as_deref()
                    .and_then(|email| normalized_account_email(Some(email)))
                    .and_then(|identity| cached_by_identity.get(&(candidate.provider_id.clone(), identity)).cloned());
                if let Some(email) = identity_email {
                    let snapshot = cached_snapshot.unwrap_or_else(|| UsageSnapshot {
                        email: Some(email.clone()),
                        bucket_name: None,
                        windows: Vec::new(),
                        reset_available_count: 0,
                        reset_credits: Vec::new(),
                    });
                    eprintln!(
                        "discovery: matched moved account {email} at {}; updating its path despite the usage error",
                        candidate.home.display()
                    );
                    let id = account_id(&candidate.provider_id, &candidate.home);
                    apply_snapshot(candidate.provider_id, candidate.home, snapshot, None, &accounts, &config);
                    apply_refresh_error(&id, user_message, &accounts);
                    schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
                    continue;
                }
                if candidate.provider_id == providers::OPENAI {
                    // A shallow candidate may be an incomplete/old Codex directory. It is
                    // intentionally not persisted as an account unless Codex validates it.
                    eprintln!("discovery: ignoring {}: {error}", candidate.home.display());
                    continue;
                }
                let snapshot = UsageSnapshot {
                    email: None,
                    bucket_name: Some(providers::display_name(&candidate.provider_id).into()),
                    windows: Vec::new(),
                    reset_available_count: 0,
                    reset_credits: Vec::new(),
                };
                let id = account_id(&candidate.provider_id, &candidate.home);
                apply_snapshot(candidate.provider_id, candidate.home, snapshot, None, &accounts, &config);
                apply_refresh_error(&id, user_message, &accounts);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
        }
    }
}

async fn perform_reset(
    codex_path: PathBuf,
    account_id: String,
    credit_id: String,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    ui: slint::Weak<MainWindow>,
    last_anchor: Arc<Mutex<Option<PanelAnchor>>>,
) {
    match config::load_pending_reset() {
        Ok(Some(_)) => {
            eprintln!("reset: a previous reset transaction still needs reconciliation; refusing to create a new idempotency key");
            let _ = ui.upgrade_in_event_loop(|ui| ui.set_reset_busy(false));
            return;
        }
        Err(error) => {
            eprintln!("reset: pending transaction journal is unreadable; refusing a potentially duplicate redemption: {error}");
            let _ = ui.upgrade_in_event_loop(|ui| ui.set_reset_busy(false));
            return;
        }
        Ok(None) => {}
    }

    let home = accounts
        .lock()
        .ok()
        .and_then(|records| records.iter().find(|record| record.id == account_id).map(|record| record.home.clone()));
    let Some(home) = home else {
        eprintln!("reset: account disappeared before confirmation: {account_id}");
        let _ = ui.upgrade_in_event_loop(|ui| ui.set_reset_busy(false));
        return;
    };

    let selected_credit_id = if credit_id == "__next__" || credit_id.is_empty() {
        None
    } else {
        Some(credit_id.clone())
    };
    let pending = PendingReset {
        account_id: account_id.clone(),
        codex_home: home.clone(),
        credit_id: selected_credit_id,
        idempotency_key: uuid::Uuid::new_v4().to_string(),
        started_at_unix: chrono::Utc::now().timestamp(),
    };
    if let Err(error) = config::save_pending_reset(&pending) {
        eprintln!("reset: refusing to submit because intent journal could not be persisted: {error}");
        let _ = ui.upgrade_in_event_loop(|ui| ui.set_reset_busy(false));
        return;
    }

    let result = codex::consume_reset(
        &codex_path,
        &home,
        &pending.idempotency_key,
        pending.credit_id.as_deref(),
    )
    .await;

    match result {
        Ok(outcome) if matches!(outcome.as_str(), "reset" | "alreadyRedeemed" | "nothingToReset" | "noCredit") => {
            eprintln!("reset: outcome={outcome} account={account_id}");
            match codex::read_openai_account(&codex_path, &home).await {
                Ok(snapshot) => {
                    apply_snapshot(providers::OPENAI.into(), home, snapshot, None, &accounts, &config);
                    if let Ok(mut records) = accounts.lock() {
                        if let Some(record) = records.iter_mut().find(|record| record.id == account_id) {
                            record.confirm_credit_id.clear();
                        }
                    }
                    if let Err(error) = config::clear_pending_reset() {
                        eprintln!("reset: could not clear reconciled journal: {error}");
                    }
                }
                Err(error) => {
                    // Consumption is already reconciled by the idempotent outcome, but
                    // retain the journal until a fresh rate-limit read succeeds.
                    eprintln!("reset: outcome was {outcome}, but authoritative reread failed: {error}");
                }
            }
        }
        Ok(other) => {
            eprintln!("reset: unknown outcome {other:?}; preserving pending journal");
        }
        Err(error) => {
            // Ambiguous transport/protocol failure: keep the idempotency key on disk.
            eprintln!("reset: consume failed ambiguously; pending journal retained: {error}");
        }
    }

    schedule_render(ui.clone(), accounts, config, last_anchor, None);
    let _ = ui.upgrade_in_event_loop(|ui| ui.set_reset_busy(false));
}

async fn reconcile_pending_reset(
    codex_path: PathBuf,
    pending: PendingReset,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    ui: slint::Weak<MainWindow>,
    last_anchor: Arc<Mutex<Option<PanelAnchor>>>,
) {
    eprintln!("reset: reconciling pending transaction from previous run");
    match codex::consume_reset(
        &codex_path,
        &pending.codex_home,
        &pending.idempotency_key,
        pending.credit_id.as_deref(),
    )
    .await {
        Ok(outcome) if matches!(outcome.as_str(), "reset" | "alreadyRedeemed" | "nothingToReset" | "noCredit") => {
            match codex::read_openai_account(&codex_path, &pending.codex_home).await {
                Ok(snapshot) => {
                    apply_snapshot(
                        providers::OPENAI.into(),
                        pending.codex_home,
                        snapshot,
                        None,
                        &accounts,
                        &config,
                    );
                    let _ = config::clear_pending_reset();
                    schedule_render(ui, accounts, config, last_anchor, None);
                }
                Err(error) => eprintln!("reset recovery: authoritative reread failed: {error}"),
            }
        }
        Ok(outcome) => eprintln!("reset recovery: unknown outcome {outcome:?}; journal retained"),
        Err(error) => eprintln!("reset recovery: retry failed; journal retained: {error}"),
    }
}

fn persist_config(config: &Arc<Mutex<AppConfig>>) {
    if let Ok(cfg) = config.lock() {
        if let Err(error) = config::save(&cfg) {
            eprintln!("settings: save failed: {error}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    ui: slint::Weak<MainWindow>,
    mut rx: UnboundedReceiver<WorkerCommand>,
    tx: UnboundedSender<WorkerCommand>,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    last_anchor_shared: Arc<Mutex<Option<PanelAnchor>>>,
    native_xid_shared: Arc<Mutex<Option<u32>>>,
    panel_visible: Arc<AtomicBool>,
    open_on_start: bool,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("agents-usage-worker".into())
        .spawn(move || {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build Tokio current-thread runtime");

            runtime.block_on(async move {
                #[cfg(not(target_os = "linux"))]
                let _ = open_on_start;
                let codex_path = match codex::locate_codex(config.lock().ok().and_then(|cfg| cfg.codex_executable.clone()).as_deref()) {
                    Ok(path) => {
                        Some(path)
                    }
                    Err(error) => {
                        eprintln!("Codex unavailable: {error}");
                        None
                    }
                };

                #[cfg(target_os = "linux")]
                let bridge = GnomeBridge { tx: tx.clone(), visible: panel_visible.clone() };
                #[cfg(target_os = "linux")]
                let dbus_connection = match zbus::connection::Builder::session()
                    .and_then(|builder| builder.name(DBUS_NAME))
                    .map(|builder| {
                        builder
                            .allow_name_replacements(false)
                            .replace_existing_names(false)
                    })
                    .and_then(|builder| builder.serve_at(DBUS_PATH, bridge))
                {
                    Ok(builder) => match builder.build().await {
                        Ok(connection) => {
                            eprintln!("GNOME bridge D-Bus service ready: {DBUS_NAME}{DBUS_PATH}");
                            Some(connection)
                        }
                        Err(error) => { eprintln!("GNOME bridge D-Bus service failed: {error}"); None }
                    },
                    Err(error) => { eprintln!("GNOME bridge D-Bus setup failed: {error}"); None }
                };

                #[cfg(target_os = "linux")]
                if dbus_connection.is_none() {
                    if activate_existing_instance_async(open_on_start).await.unwrap_or(false) {
                        let _ = slint::invoke_from_event_loop(|| { let _ = slint::quit_event_loop(); });
                        return;
                    }
                    eprintln!("GNOME bridge is unavailable; preserving requested hidden/visible launch mode");
                }

                #[cfg(not(target_os = "linux"))]
                let dbus_connection = Some(());

                #[cfg(target_os = "linux")]
                let _status_notifier = if should_use_status_notifier() {
                    use ksni::TrayMethods as _;
                    match (StatusNotifierTray { tx: tx.clone() })
                        .assume_sni_available(true)
                        .spawn()
                        .await
                    {
                        Ok(handle) => {
                            eprintln!("StatusNotifierItem tray ready");
                            Some(handle)
                        }
                        Err(error) => {
                            eprintln!("StatusNotifierItem tray unavailable: {error}");
                            None
                        }
                    }
                } else {
                    None
                };

                let tick_tx = tx.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(60));
                    // `interval` ticks immediately once; the cached state was already
                    // rendered during startup, so consume that tick without redrawing.
                    interval.tick().await;
                    loop {
                        interval.tick().await;
                        if tick_tx.send(WorkerCommand::Tick).is_err() { break; }
                    }
                });

                #[cfg(target_os = "linux")]
                {
                    let focus_tx = tx.clone();
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval(Duration::from_millis(200));
                        loop {
                            interval.tick().await;
                            if focus_tx.send(WorkerCommand::CheckPopupFocus).is_err() { break; }
                        }
                    });
                }

                let refreshing = Arc::new(AtomicBool::new(false));
                let refresh_pending = Arc::new(AtomicBool::new(false));
                let discovering = Arc::new(AtomicBool::new(false));
                let rpc_semaphore = Arc::new(Semaphore::new(MAX_RPC_CONCURRENCY));
                let last_data_refresh = Arc::new(Mutex::new(None::<Instant>));

                let mut mobile_server = start_mobile_server(
                    accounts.clone(), config.clone(), refreshing.clone(), tx.clone(), ui.clone(),
                );
                #[cfg(target_os = "linux")]
                let popup_focus_seen = Arc::new(AtomicBool::new(false));

                while let Some(command) = rx.recv().await {
                    match command {
                        #[cfg(target_os = "linux")]
                        WorkerCommand::ToggleAt(anchor) => {
                            if let Ok(mut shared) = last_anchor_shared.lock() { *shared = Some(anchor); }
                            let opening = !panel_visible.load(Ordering::SeqCst);
                            panel_visible.store(opening, Ordering::SeqCst);
                            if opening {
                                popup_focus_seen.store(false, Ordering::SeqCst);
                                schedule_show_dashboard(
                                    ui.clone(), Some(anchor), native_xid_shared.clone(), panel_visible.clone(),
                                );
                            } else {
                                let ui_weak = ui.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_weak.upgrade() { let _ = ui.hide(); }
                                });
                            }
                            if opening { send(&tx, WorkerCommand::RefreshIfStale); }
                        }
                        #[cfg(target_os = "linux")]
                        WorkerCommand::OpenAt(anchor) => {
                            if let Ok(mut shared) = last_anchor_shared.lock() { *shared = Some(anchor); }
                            panel_visible.store(true, Ordering::SeqCst);
                            popup_focus_seen.store(false, Ordering::SeqCst);
                            schedule_show_dashboard(
                                ui.clone(), Some(anchor), native_xid_shared.clone(), panel_visible.clone(),
                            );
                            send(&tx, WorkerCommand::RefreshIfStale);
                        }
                        #[cfg(target_os = "linux")]
                        WorkerCommand::OpenSettingsAt(anchor) => {
                            if let Ok(mut shared) = last_anchor_shared.lock() { *shared = Some(anchor); }
                            panel_visible.store(true, Ordering::SeqCst);
                            popup_focus_seen.store(false, Ordering::SeqCst);
                            schedule_show_settings(
                                ui.clone(), Some(anchor), native_xid_shared.clone(), panel_visible.clone(),
                            );
                            send(&tx, WorkerCommand::RefreshIfStale);
                        }
                        WorkerCommand::OpenSettings => {
                            panel_visible.store(true, Ordering::SeqCst);
                            #[cfg(target_os = "linux")]
                            popup_focus_seen.store(false, Ordering::SeqCst);
                            let anchor = last_anchor_shared.lock().ok().and_then(|value| *value);
                            schedule_show_settings(
                                ui.clone(), anchor, native_xid_shared.clone(), panel_visible.clone(),
                            );
                        }
                        WorkerCommand::OpenStandalone => {
                            panel_visible.store(true, Ordering::SeqCst);
                            #[cfg(target_os = "linux")]
                            popup_focus_seen.store(false, Ordering::SeqCst);
                            let anchor = last_anchor_shared.lock().ok().and_then(|value| *value);
                            schedule_show_dashboard(
                                ui.clone(), anchor, native_xid_shared.clone(), panel_visible.clone(),
                            );
                            send(&tx, WorkerCommand::RefreshIfStale);
                        }
                        WorkerCommand::ToggleAtPoint { x, y, icon_w, icon_h } => {
                            let opening = !panel_visible.load(Ordering::SeqCst);
                            panel_visible.store(opening, Ordering::SeqCst);
                            #[cfg(target_os = "linux")]
                            if opening { popup_focus_seen.store(false, Ordering::SeqCst); }
                            let ui_weak = ui.clone();
                            let native_xid = native_xid_shared.clone();
                            let last_anchor = last_anchor_shared.clone();
                            let visible_shared = panel_visible.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                let Some(ui) = ui_weak.upgrade() else { return; };
                                if !opening {
                                    let _ = ui.hide();
                                    return;
                                }
                                // The first tray click may arrive before Slint has created its
                                // hidden native window. Await it on the UI loop, then resolve the
                                // correct monitor and position before the first visible show.
                                let _ = slint::spawn_local(async move {
                                    use slint::winit_030::WinitWindowAccessor as _;
                                    let _ = ui.window().winit_window().await;
                                    if !visible_shared.load(Ordering::SeqCst) { return; }
                                    let anchor = anchor_for_screen_point(&ui, x, y, icon_w, icon_h);
                                    if let Ok(mut shared) = last_anchor.lock() { *shared = anchor; }
                                    show_dashboard(&ui, anchor, &native_xid);
                                });
                            });
                            if opening { send(&tx, WorkerCommand::RefreshIfStale); }
                        }
                        refresh_command @ (WorkerCommand::Refresh | WorkerCommand::RefreshIfStale | WorkerCommand::RefreshIfStaleMobile) => {
                            let force = matches!(refresh_command, WorkerCommand::Refresh);
                            if !force {
                                let freshness = if matches!(refresh_command, WorkerCommand::RefreshIfStaleMobile) {
                                    MOBILE_REFRESH_FRESHNESS
                                } else {
                                    OPEN_REFRESH_FRESHNESS
                                };
                                let still_fresh = last_data_refresh
                                    .lock()
                                    .ok()
                                    .and_then(|value| *value)
                                .map(|when| when.elapsed() < freshness)
                                    .unwrap_or(false);
                                if still_fresh {
                                    eprintln!("refresh: skipped because account data is less than {}s old", freshness.as_secs());
                                    continue;
                                }
                            }
                            if refreshing.swap(true, Ordering::SeqCst) {
                                if force { refresh_pending.store(true, Ordering::SeqCst); }
                                continue;
                            }
                            let _ = ui.upgrade_in_event_loop(|ui| ui.set_refreshing(true));
                            let accounts2 = accounts.clone();
                            let config2 = config.clone();
                            let ui2 = ui.clone();
                            let anchor2 = last_anchor_shared.clone();
                            let refreshing2 = refreshing.clone();
                            let refresh_pending2 = refresh_pending.clone();
                            let discovering2 = discovering.clone();
                            let rpc_semaphore2 = rpc_semaphore.clone();
                            let tx2 = tx.clone();
                            let last_data_refresh2 = last_data_refresh.clone();
                            let codex_path2 = codex_path.clone();
                            tokio::spawn(async move {
                                // A pending reset is reconciled only as part of an explicit
                                // open/refresh action. Background launches stay read-only.
                                if let (Some(codex_path), Ok(Some(pending))) =
                                    (codex_path2.clone(), config::load_pending_reset())
                                {
                                    reconcile_pending_reset(codex_path, pending, accounts2.clone(), config2.clone(), ui2.clone(), anchor2.clone()).await;
                                }
                                let had_known_accounts = accounts2
                                    .lock()
                                    .map(|records| records.iter().any(|record| record.enabled))
                                    .unwrap_or(false);

                                // Fast lane: refresh already-known enabled accounts first.
                                refresh_known_accounts(
                                    codex_path2.clone(), accounts2.clone(), config2.clone(), ui2.clone(), anchor2.clone(),
                                    rpc_semaphore2.clone(),
                                    INTERACTIVE_REFRESH_CONCURRENCY,
                                ).await;

                                // Once known accounts are current, the visible refresh operation is done.
                                // Discovery is deliberately lower priority and must not keep the button blocked.
                                if had_known_accounts {
                                    if let Ok(mut value) = last_data_refresh2.lock() { *value = Some(Instant::now()); }
                                    refreshing2.store(false, Ordering::SeqCst);
                                    let _ = ui2.upgrade_in_event_loop(|ui| ui.set_refreshing(false));
                                }

                                if !discovering2.swap(true, Ordering::SeqCst) {
                                    discover_new_accounts(
                                        codex_path2, accounts2, config2, ui2.clone(), anchor2,
                                        rpc_semaphore2,
                                        INTERACTIVE_DISCOVERY_CONCURRENCY,
                                    ).await;
                                    discovering2.store(false, Ordering::SeqCst);
                                } else {
                                    eprintln!("discovery: skipped because another discovery pass is still running");
                                }

                                // On a first run there were no known accounts, so discovery *is* the
                                // initial refresh and the button stays disabled until that finishes.
                                if !had_known_accounts {
                                    if let Ok(mut value) = last_data_refresh2.lock() { *value = Some(Instant::now()); }
                                    refreshing2.store(false, Ordering::SeqCst);
                                    let _ = ui2.upgrade_in_event_loop(|ui| ui.set_refreshing(false));
                                }
                                if refresh_pending2.swap(false, Ordering::SeqCst) {
                                    send(&tx2, WorkerCommand::Refresh);
                                }
                            });
                        }
                        WorkerCommand::PersistSettings => persist_config(&config),
                        WorkerCommand::MobileConfigChanged => {
                            persist_config(&config);
                            if let Some(server) = mobile_server.take() {
                                server.stop().await;
                            }
                            mobile_server = start_mobile_server(
                                accounts.clone(), config.clone(), refreshing.clone(), tx.clone(), ui.clone(),
                            );
                            schedule_render(
                                ui.clone(), accounts.clone(), config.clone(), last_anchor_shared.clone(), None,
                            );
                        }
                        WorkerCommand::MobileDeviceListChanged => {
                            schedule_render(
                                ui.clone(), accounts.clone(), config.clone(), last_anchor_shared.clone(), None,
                            );
                        }
                        WorkerCommand::ConsumeReset { account_id, credit_id } => {
                            let Some(codex_path) = codex_path.clone() else { continue; };
                            let _ = ui.upgrade_in_event_loop(|ui| ui.set_reset_busy(true));
                            tokio::spawn(perform_reset(
                                codex_path, account_id, credit_id,
                                accounts.clone(), config.clone(), ui.clone(), last_anchor_shared.clone(),
                            ));
                        }
                        WorkerCommand::HidePanel => {
                            panel_visible.store(false, Ordering::SeqCst);
                            #[cfg(target_os = "linux")]
                            popup_focus_seen.store(false, Ordering::SeqCst);
                            let _ = ui.upgrade_in_event_loop(|ui| { let _ = ui.hide(); });
                        }
                        #[cfg(target_os = "linux")]
                        WorkerCommand::CheckPopupFocus => {
                            if !panel_visible.load(Ordering::SeqCst) { continue; }
                            let focused = native_xid_shared
                                .lock()
                                .ok()
                                .and_then(|guard| *guard)
                                .is_some_and(x11_popup_has_input_focus);
                            if focused {
                                popup_focus_seen.store(true, Ordering::SeqCst);
                            } else if popup_focus_seen.load(Ordering::SeqCst) {
                                panel_visible.store(false, Ordering::SeqCst);
                                popup_focus_seen.store(false, Ordering::SeqCst);
                                let _ = ui.upgrade_in_event_loop(|ui| { let _ = ui.hide(); });
                            }
                        }
                        WorkerCommand::Tick => {
                            schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor_shared.clone(), None);
                        }
                        WorkerCommand::Quit => {
                            eprintln!("worker: quit requested");
                            break;
                        }
                    }
                }

                if let Some(server) = mobile_server.take() {
                    server.stop().await;
                }
                persist_config(&config);

                panel_visible.store(false, Ordering::SeqCst);
                #[cfg(target_os = "linux")]
                drop(dbus_connection);
                #[cfg(not(target_os = "linux"))]
                let _ = dbus_connection;
                let _ = slint::invoke_from_event_loop(|| { let _ = slint::quit_event_loop(); });
            });
        })
        .expect("spawn worker thread")
}

#[cfg(target_os = "linux")]
async fn activate_existing_instance_async(open: bool) -> Result<bool, zbus::Error> {
    let connection = zbus::Connection::session().await?;
    let dbus = zbus::fdo::DBusProxy::new(&connection).await?;
    let name = zbus::names::BusName::try_from(DBUS_NAME)?;
    if !dbus.name_has_owner(name).await? {
        return Ok(false);
    }

    if open {
        let proxy = zbus::Proxy::new(&connection, DBUS_NAME, DBUS_PATH, "io.github.agentsusagetray.GnomeBridge1").await?;
        let _: bool = proxy.call("Open", &()).await?;
        eprintln!("activated existing Agents Usage instance");
    } else {
        eprintln!("Agents Usage is already running; background launch is complete");
    }
    Ok(true)
}

#[cfg(target_os = "linux")]
fn activate_existing_instance(open: bool) -> bool {
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("single-instance check unavailable: {error}");
            return false;
        }
    };

    match runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(2),
            activate_existing_instance_async(open),
        ).await
    }) {
        Ok(Ok(active)) => active,
        Ok(Err(error)) => {
            eprintln!("single-instance check failed: {error}");
            false
        }
        Err(_) => {
            eprintln!("single-instance check timed out");
            false
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
    #[cfg(target_os = "linux")]
    use slint::winit_030::winit::platform::x11::{EventLoopBuilderExtX11 as _, WindowAttributesExtX11, WindowType};

    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if handle_mobile_command(&arguments) { return Ok(()); }
    let open_on_start = launch_mode(arguments) == LaunchMode::Open;
    #[cfg(target_os = "linux")]
    if activate_existing_instance(open_on_start) {
        return Ok(());
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let _instance_lock = match acquire_native_instance_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            eprintln!("Agents Usage is already running");
            return Ok(());
        }
        Err(error) => {
            eprintln!("single-instance lock unavailable: {error}");
            return Ok(());
        }
    };

    #[allow(unused_mut)]
    let mut event_loop_builder =
        winit::event_loop::EventLoop::<slint::winit_030::SlintEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    event_loop_builder.with_x11();

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .with_winit_event_loop_builder(event_loop_builder)
        .with_winit_window_attributes_hook(|attributes| {
            let attributes = attributes
                .with_visible(false)
                .with_decorations(false)
                .with_resizable(false);
            #[cfg(target_os = "linux")]
            return attributes
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::PopupMenu])
                .with_name("agents-usage-popover", "agents-usage-popover");
            #[cfg(not(target_os = "linux"))]
            attributes
        })
        .select()?;

    let ui = MainWindow::new()?;
    ui.window().on_close_requested(|| CloseRequestResponse::HideWindow);

    let mut loaded_config = config::load();
    let migrated_display_preferences = config::migrate_display_preferences(&mut loaded_config);
    let migrated_mobile_access = mobile::migrate_legacy_access(&mut loaded_config);
    let migrated_persistent_sessions = mobile::make_device_sessions_persistent(&mut loaded_config);
    if migrated_display_preferences || migrated_mobile_access || migrated_persistent_sessions {
        if let Err(error) = config::save(&loaded_config) {
            eprintln!("mobile: could not persist the phone-session migration: {error}");
        }
    }
    let loaded_cache = config::load_usage_cache();
    let (loaded_cache, reconciled) = reconcile_cached_accounts(&mut loaded_config, loaded_cache);
    if reconciled {
        if let Err(error) = config::save(&loaded_config) {
            eprintln!("settings: could not persist reconciled account paths: {error}");
        }
        if let Err(error) = config::save_usage_cache(&loaded_cache) {
            eprintln!("cache: could not persist reconciled account paths: {error}");
        }
    }
    let cached_usage = loaded_cache
        .into_iter()
        .map(|cached| ((cached.provider_id, canonical_id(&cached.home)), cached.snapshot))
        .collect::<HashMap<_, _>>();
    let initial_records = loaded_config
        .accounts
        .iter()
        .enumerate()
        .filter_map(|(index, pref)| {
            placeholder_record(
                pref,
                index,
                cached_usage.get(&(pref.provider_id.clone(), canonical_id(&pref.home))).cloned(),
            )
        })
        .collect::<Vec<_>>();
    let restored_usage_count = initial_records
        .iter()
        .filter(|record| record.snapshot.is_some())
        .count();
    eprintln!(
        "cache: restored {restored_usage_count}/{} configured account snapshot(s)",
        initial_records.len()
    );

    let accounts = Arc::new(Mutex::new(initial_records));
    let config = Arc::new(Mutex::new(loaded_config));
    let last_anchor_shared = Arc::new(Mutex::new(None::<PanelAnchor>));
    let native_xid_shared = Arc::new(Mutex::new(None::<u32>));
    let panel_visible = Arc::new(AtomicBool::new(false));
    let (tx, rx) = unbounded_channel::<WorkerCommand>();

    let initial_mobile_endpoints = discover_mobile_endpoints(
        config.lock().ok().map(|cfg| cfg.mobile.port).unwrap_or(3765),
    );
    ui.set_mobile_lan_url(initial_mobile_endpoints.lan.unwrap_or_default().into());
    ui.set_mobile_tailscale_url(initial_mobile_endpoints.tailscale.unwrap_or_default().into());

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let _native_tray = match create_native_tray(tx.clone()) {
        Ok(tray) => Some(tray),
        Err(error) => {
            eprintln!("native tray creation failed: {error}");
            send(&tx, WorkerCommand::OpenStandalone);
            None
        }
    };

    render_ui(&ui, &accounts, &config, &last_anchor_shared, Some("Looking for agent accounts…"));

    {
        let tx = tx.clone();
        ui.on_refresh_requested(move || send(&tx, WorkerCommand::Refresh));
    }
    {
        let tx = tx.clone();
        ui.on_settings_requested(move || send(&tx, WorkerCommand::OpenSettings));
    }
    {
        let ui_weak = ui.as_weak();
        let config = config.clone();
        let tx = tx.clone();
        ui.on_mobile_enabled_changed(move |enabled| {
            if let Ok(mut config) = config.lock() {
                config.mobile.enabled = enabled;
                if !enabled {
                    config.mobile.pairing = None;
                }
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_mobile_status(if enabled { "Starting…".into() } else { "Disabled".into() });
                if !enabled {
                    ui.set_mobile_pairing_link("".into());
                    ui.set_mobile_qr_cells(ModelRc::default());
                }
            }
            send(&tx, WorkerCommand::MobileConfigChanged);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let config = config.clone();
        let tx = tx.clone();
        ui.on_mobile_allow_lan_changed(move |allow| {
            if let Ok(mut config) = config.lock() {
                config.mobile.allow_lan = Some(allow);
                config.mobile.bind = if allow { "0.0.0.0".into() } else { "127.0.0.1".into() };
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_mobile_status(
                    if allow { "Restarting with private LAN access…".into() }
                    else { "Restarting in Tailscale-only mode…".into() },
                );
            }
            send(&tx, WorkerCommand::MobileConfigChanged);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let config = config.clone();
        let tx = tx.clone();
        ui.on_mobile_create_pairing(move || {
            let port = config.lock().ok().map(|cfg| cfg.mobile.port).unwrap_or(3765);
            let allow_lan = config.lock().ok().is_some_and(|cfg| cfg.mobile.allows_lan());
            let mut endpoints = discover_mobile_endpoints(port);
            if !allow_lan { endpoints.lan = None; }
            let uses = u8::from(endpoints.lan.is_some()) + u8::from(endpoints.tailscale.is_some());
            if uses == 0 {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_mobile_status("Choose Allow direct LAN or set up Tailscale, then show the QR.".into());
                }
                return;
            }
            let token = if let Ok(mut config) = config.lock() {
                if !config.mobile.enabled {
                    config.mobile.enabled = true;
                }
                mobile::create_pairing(&mut config, uses)
            } else {
                return;
            };
            let Ok(bundle) = mobile_pairing_bundle(&endpoints, &token) else {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_mobile_status("The detected phone addresses were invalid.".into());
                }
                return;
            };
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_mobile_lan_url(endpoints.lan.unwrap_or_default().into());
                ui.set_mobile_tailscale_url(endpoints.tailscale.unwrap_or_default().into());
                ui.set_mobile_pairing_link(bundle.clone().into());
                ui.set_mobile_qr_cells(qr_cell_model(&bundle));
                ui.set_mobile_status("QR ready · scan it with the phone camera".into());
            }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_mobile_copy_pairing(move || {
            let Some(ui) = ui_weak.upgrade() else { return; };
            let link = ui.get_mobile_pairing_link();
            if link.is_empty() { return; }
            match ClipboardContext::new().and_then(|mut clipboard| clipboard.set_contents(link.to_string())) {
                Ok(()) => ui.set_mobile_status("Private pairing link copied".into()),
                Err(_) => ui.set_mobile_status("Could not access the system clipboard.".into()),
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let config = config.clone();
        ui.on_mobile_configure_tailscale(move || {
            let ui_weak = ui_weak.clone();
            let port = config.lock().ok().map(|cfg| cfg.mobile.port).unwrap_or(3765);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_mobile_status("Configuring private Tailscale HTTPS…".into());
            }
            thread::spawn(move || {
                let target = format!("http://127.0.0.1:{port}");
                let result = std::process::Command::new("tailscale")
                    .args(["serve", "--yes", "--bg", "--set-path", "/agents-usage", &target])
                    .output();
                let endpoints = discover_mobile_endpoints(port);
                let status = match result {
                    Ok(output) if output.status.success() && endpoints.tailscale.is_some() => {
                        "Tailscale HTTPS is configured and verified.".to_string()
                    }
                    Ok(output) if output.status.success() => {
                        "Tailscale accepted the setup, but its HTTPS route could not be verified.".to_string()
                    }
                    Ok(output) => {
                        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        if message.is_empty() {
                            format!("Tailscale setup failed with {}.", output.status)
                        } else {
                            format!("Tailscale setup needs attention: {message}")
                        }
                    }
                    Err(_) => "Tailscale was not found. Install it, sign in, then try again.".into(),
                };
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_mobile_lan_url(endpoints.lan.unwrap_or_default().into());
                    ui.set_mobile_tailscale_url(endpoints.tailscale.unwrap_or_default().into());
                    ui.set_mobile_status(status.into());
                });
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_mobile_revoke_device(move |id| {
            let changed = config.lock().is_ok_and(|mut config| mobile::revoke_device(&mut config, id.as_str()));
            if changed {
                send(&tx, WorkerCommand::PersistSettings);
                if let Some(ui) = ui_weak.upgrade() {
                    render_ui(&ui, &accounts, &config, &last_anchor, None);
                    ui.set_mobile_status("Phone access revoked".into());
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        ui.on_back_to_dashboard(move || {
            if let Some(ui) = ui_weak.upgrade() {
                render_ui(&ui, &accounts, &config, &last_anchor, None);
            }
        });
    }
    {
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let tx = tx.clone();
        ui.on_account_custom_color_changed(move |id, color| {
            let color = color_hex(color);
            let home = if let Ok(mut records) = accounts.lock() {
                records.iter_mut().find(|record| record.id == id.as_str()).map(|record| {
                    record.color_name = color.clone();
                    (record.provider_id.clone(), record.home.clone())
                })
            } else { None };
            if let Some((provider_id, home)) = home {
                if let Ok(mut cfg) = config_arc.lock() {
                    config::preference_for_provider_mut(&mut cfg, &provider_id, &home).color = Some(color);
                }
                send(&tx, WorkerCommand::PersistSettings);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_account_toggle_details(move |id| {
            let changed = if let Ok(mut records) = accounts.lock() {
                if let Some(record) = records.iter_mut().find(|record| record.id == id.as_str()) {
                    record.expanded = !record.expanded;
                    if !record.expanded { record.confirm_credit_id.clear(); }
                    Some((record.provider_id.clone(), record.home.clone(), record.expanded))
                } else {
                    None
                }
            } else { None };
            if let Some((provider_id, home, expanded)) = changed {
                if let Ok(mut cfg) = config.lock() {
                    config::preference_for_provider_mut(&mut cfg, &provider_id, &home).expanded = expanded;
                }
                send(&tx, WorkerCommand::PersistSettings);
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config, &last_anchor, None); }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        ui.on_account_toggle_name(move |id| {
            if let Ok(mut records) = accounts.lock() {
                if let Some(record) = records.iter_mut().find(|record| record.id == id.as_str()) {
                    record.name_revealed = !record.name_revealed;
                }
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config, &last_anchor, None); }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        ui.on_account_toggle_email(move |id| {
            if let Ok(mut records) = accounts.lock() {
                if let Some(record) = records.iter_mut().find(|record| record.id == id.as_str()) {
                    record.email_revealed = !record.email_revealed;
                }
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config, &last_anchor, None); }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        ui.on_account_arm_reset(move |id, credit| {
            if let Ok(mut records) = accounts.lock() {
                if let Some(record) = records.iter_mut().find(|record| record.id == id.as_str()) {
                    record.confirm_credit_id = credit.to_string();
                }
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config, &last_anchor, None); }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config = config.clone();
        let last_anchor = last_anchor_shared.clone();
        ui.on_account_cancel_reset(move |id| {
            if let Ok(mut records) = accounts.lock() {
                if let Some(record) = records.iter_mut().find(|record| record.id == id.as_str()) {
                    record.confirm_credit_id.clear();
                }
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config, &last_anchor, None); }
        });
    }
    {
        let tx = tx.clone();
        ui.on_account_confirm_reset(move |id, credit| {
            send(&tx, WorkerCommand::ConsumeReset { account_id: id.to_string(), credit_id: credit.to_string() });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_blur_emails_changed(move |value| {
            if let Ok(mut cfg) = config_arc.lock() { cfg.blur_emails = value; }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_blur_names_changed(move |value| {
            if let Ok(mut cfg) = config_arc.lock() { cfg.blur_names = value; }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_color_reset_timers_changed(move |value| {
            if let Ok(mut cfg) = config_arc.lock() { cfg.color_reset_timers = value; }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_usage_bar_color_mode_changed(move |value| {
            let Some(mode) = UsageBarColorMode::parse(value.as_str()) else { return; };
            if let Ok(mut cfg) = config_arc.lock() { cfg.usage_bar_color_mode = mode; }
            if let Some(ui) = ui_weak.upgrade() {
                render_ui(&ui, &accounts, &config_arc, &last_anchor, None);
            }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_usage_bar_custom_color_changed(move |value| {
            if let Ok(mut cfg) = config_arc.lock() {
                cfg.usage_bar_custom_color = color_hex(value);
                cfg.usage_bar_color_mode = UsageBarColorMode::Custom;
            }
            if let Some(ui) = ui_weak.upgrade() {
                render_ui(&ui, &accounts, &config_arc, &last_anchor, None);
            }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_always_show_reset_counter_changed(move |value| {
            if let Ok(mut cfg) = config_arc.lock() {
                cfg.always_show_reset_counter = value;
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_show_banked_resets_changed(move |value| {
            if let Ok(mut cfg) = config_arc.lock() {
                cfg.show_banked_resets = value;
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_account_enabled_changed(move |id, value| {
            let home = if let Ok(mut records) = accounts.lock() {
                records.iter_mut().find(|record| record.id == id.as_str()).map(|record| {
                    record.enabled = value;
                    (record.provider_id.clone(), record.home.clone())
                })
            } else { None };
            if let Some((provider_id, home)) = home {
                if let Ok(mut cfg) = config_arc.lock() { config::preference_for_provider_mut(&mut cfg, &provider_id, &home).enabled = value; }
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
            if value { send(&tx, WorkerCommand::Refresh); }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_account_pin_short_changed(move |id, value| {
            let home = if let Ok(mut records) = accounts.lock() {
                records.iter_mut().find(|record| record.id == id.as_str()).map(|record| {
                    record.pin_short = value;
                    (record.provider_id.clone(), record.home.clone())
                })
            } else { None };
            if let Some((provider_id, home)) = home {
                if let Ok(mut cfg) = config_arc.lock() { config::preference_for_provider_mut(&mut cfg, &provider_id, &home).pin_short = value; }
            }
            if let Some(ui) = ui_weak.upgrade() { render_ui(&ui, &accounts, &config_arc, &last_anchor, None); }
            send(&tx, WorkerCommand::PersistSettings);
        });
    }
    {
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let tx = tx.clone();
        ui.on_account_name_changed(move |id, value| {
            let requested_name = normalized_display_name(value.as_str());
            let home = if let Ok(mut records) = accounts.lock() {
                records.iter_mut().find(|record| record.id == id.as_str()).map(|record| {
                    record.display_name = requested_name.clone().unwrap_or_else(|| default_account_name(record));
                    (record.provider_id.clone(), record.home.clone())
                })
            } else { None };
            if let Some((provider_id, home)) = home {
                if let Ok(mut cfg) = config_arc.lock() {
                    config::preference_for_provider_mut(&mut cfg, &provider_id, &home).display_name = requested_name;
                }
                send(&tx, WorkerCommand::PersistSettings);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_account_color_changed(move |id, color| {
            let color = color.as_str();
            if !ui_model::is_account_color(color) { return; }
            let home = if let Ok(mut records) = accounts.lock() {
                records.iter_mut().find(|record| record.id == id.as_str()).map(|record| {
                    record.color_name = color.to_string();
                    (record.provider_id.clone(), record.home.clone())
                })
            } else { None };
            if let Some((provider_id, home)) = home {
                if let Ok(mut cfg) = config_arc.lock() {
                    config::preference_for_provider_mut(&mut cfg, &provider_id, &home).color = Some(color.to_string());
                }
                if let Some(ui) = ui_weak.upgrade() {
                    render_ui(&ui, &accounts, &config_arc, &last_anchor, None);
                }
                send(&tx, WorkerCommand::PersistSettings);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let accounts = accounts.clone();
        let config_arc = config.clone();
        let last_anchor = last_anchor_shared.clone();
        let tx = tx.clone();
        ui.on_account_move_requested(move |id, direction| {
            let moved = if let Ok(mut records) = accounts.lock() {
                if let Ok(mut cfg) = config_arc.lock() {
                    move_account(&mut records, &mut cfg, id.as_str(), direction)
                } else { false }
            } else { false };
            if moved {
                if let Some(ui) = ui_weak.upgrade() {
                    render_ui(&ui, &accounts, &config_arc, &last_anchor, None);
                }
                send(&tx, WorkerCommand::PersistSettings);
            }
        });
    }

    // Real popover behavior: Escape and an actual focus loss dismiss it.
    {
        let tx = tx.clone();
        ui.window().on_winit_window_event(move |_slint_window, event| {
            match event {
                winit::event::WindowEvent::Focused(focused) => {
                    #[cfg(target_os = "linux")]
                    if !*focused { send(&tx, WorkerCommand::CheckPopupFocus); }
                    #[cfg(not(target_os = "linux"))]
                    if !*focused { send(&tx, WorkerCommand::HidePanel); }
                }
                winit::event::WindowEvent::KeyboardInput { event, .. }
                    if event.state == winit::event::ElementState::Pressed
                        && matches!(event.logical_key, winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)) =>
                {
                    send(&tx, WorkerCommand::HidePanel);
                    return EventResult::PreventDefault;
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }

    let worker = spawn_worker(
        ui.as_weak(), rx, tx.clone(),
        accounts, config, last_anchor_shared, native_xid_shared, panel_visible, open_on_start,
    );
    if open_on_start {
        send(&tx, WorkerCommand::OpenStandalone);
    }
    let result = slint::run_event_loop_until_quit();
    eprintln!("Slint event loop exited: {result:?}");
    send(&tx, WorkerCommand::Quit);
    drop(tx);
    let _ = worker.join();
    eprintln!("Agents Usage shutdown complete");
    result
}

#[cfg(test)]
mod tests {
    use super::{
        LaunchMode, PanelAnchor, PanelEdge, SCREEN_MARGIN_PX, desktop_uses_status_notifier, infer_panel_edge,
        launch_mode, mobile_lan_url, mobile_pairing_bundle, mobile_pairing_url, move_account, move_target, normalized_account_color,
        normalized_display_name, panel_position_for_size, placeholder_record, reconcile_cached_accounts,
        tailscale_serve_matches,
    };
    use super::MobileEndpoints;
    use crate::config::{AccountPreference, AppConfig};
    use crate::domain::{CachedUsage, UsageSnapshot};
    use std::fs;

    #[test]
    fn only_an_explicit_open_argument_shows_the_window_at_launch() {
        assert_eq!(launch_mode(Vec::new()), LaunchMode::Background);
        assert_eq!(launch_mode(["--background".into()]), LaunchMode::Background);
        assert_eq!(launch_mode(["--open".into()]), LaunchMode::Open);
        assert_eq!(
            launch_mode(["--background".into(), "--open".into()]),
            LaunchMode::Open
        );
    }

    #[test]
    fn mobile_pairing_urls_are_normalized_and_validated() {
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            mobile_pairing_url(" https://desktop.example.ts.net/agents-usage/ ", token).unwrap(),
            format!("https://desktop.example.ts.net/agents-usage/pair?token={token}&path=%2Fagents-usage%2F")
        );
        assert_eq!(
            mobile_pairing_url("http://192.168.1.20:3765", token).unwrap(),
            format!("http://192.168.1.20:3765/pair?token={token}&path=%2F")
        );
        assert_eq!(
            mobile_pairing_url("http://[fd00::20]:3765/Phone%20View", token).unwrap(),
            format!("http://[fd00::20]:3765/Phone%20View/pair?token={token}&path=%2FPhone%2520View%2F")
        );
        assert!(mobile_pairing_url("ftp://desktop.example", token).is_err());
        assert!(mobile_pairing_url("https://user@desktop.example", token).is_err());
        assert!(mobile_pairing_url("https://desktop.example/path?wrong=1", token).is_err());
        assert!(mobile_pairing_url("https://desktop.example/path#wrong", token).is_err());
        assert!(mobile_pairing_url("https://desktop.example", "short").is_err());
    }

    #[test]
    fn mobile_lan_urls_bracket_ipv6_literals() {
        assert_eq!(
            mobile_lan_url("192.168.1.20".parse().unwrap(), 3765),
            "http://192.168.1.20:3765/"
        );
        assert_eq!(
            mobile_lan_url("fd00::20".parse().unwrap(), 3765),
            "http://[fd00::20]:3765/"
        );
    }

    #[test]
    fn mobile_pairing_bundle_carries_lan_and_tailscale_without_duplicating_the_token() {
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let bundle = mobile_pairing_bundle(
            &MobileEndpoints {
                lan: Some("http://192.168.1.20:3765/".into()),
                tailscale: Some("https://desktop.example.ts.net/agents-usage/".into()),
            },
            token,
        )
        .unwrap();
        assert!(bundle.starts_with("agents-usage://pair?token="));
        assert_eq!(bundle.matches(token).count(), 1);
        assert!(bundle.contains("&base=http%3A%2F%2F192.168.1.20%3A3765%2F"));
        assert!(bundle.contains("&fallback=https%3A%2F%2Fdesktop.example.ts.net%2Fagents-usage%2F"));
    }

    #[test]
    fn tailscale_route_is_only_advertised_when_serve_targets_this_companion() {
        let status = serde_json::json!({
            "Web": {
                "desktop.example.ts.net:443": {
                    "Handlers": {
                        "/agents-usage": { "Proxy": "http://127.0.0.1:3765" }
                    }
                }
            }
        });
        assert!(tailscale_serve_matches(&status, "desktop.example.ts.net", 3765));
        assert!(!tailscale_serve_matches(&status, "desktop.example.ts.net", 4000));
        assert!(!tailscale_serve_matches(&status, "other.example.ts.net", 3765));
    }

    #[test]
    fn non_gnome_linux_desktops_use_the_status_notifier_fallback() {
        assert!(!desktop_uses_status_notifier("GNOME"));
        assert!(!desktop_uses_status_notifier("ubuntu:GNOME"));
        assert!(desktop_uses_status_notifier("KDE"));
        assert!(desktop_uses_status_notifier("XFCE"));
        assert!(desktop_uses_status_notifier("X-Cinnamon"));
        assert!(desktop_uses_status_notifier(""));
    }

    #[test]
    fn popup_placement_handles_every_panel_edge_and_clamps_to_the_monitor() {
        let base = PanelAnchor {
            icon_x: 980,
            icon_y: 540,
            icon_w: 24,
            icon_h: 24,
            monitor_x: 0,
            monitor_y: 0,
            monitor_w: 1_920,
            monitor_h: 1_080,
            edge: PanelEdge::Top,
        };
        let top = panel_position_for_size(base, 360, 400);
        assert!(top.y > base.icon_y);
        let bottom = panel_position_for_size(PanelAnchor { edge: PanelEdge::Bottom, ..base }, 360, 400);
        assert!(bottom.y < base.icon_y);
        let left = panel_position_for_size(PanelAnchor { edge: PanelEdge::Left, ..base }, 360, 400);
        assert!(left.x > base.icon_x);
        let right = panel_position_for_size(PanelAnchor { edge: PanelEdge::Right, ..base }, 360, 400);
        assert!(right.x < base.icon_x);

        let corner = PanelAnchor { icon_x: -80, icon_y: -80, ..base };
        let clamped = panel_position_for_size(corner, 360, 400);
        assert_eq!(clamped.x, SCREEN_MARGIN_PX);
        assert_eq!(clamped.y, SCREEN_MARGIN_PX);
    }

    #[test]
    fn tray_geometry_detects_top_bottom_left_and_right_panels() {
        let monitor = (100, 50, 1_600, 900);
        assert!(matches!(infer_panel_edge(700, 50, 24, 24, monitor), PanelEdge::Top));
        assert!(matches!(infer_panel_edge(700, 926, 24, 24, monitor), PanelEdge::Bottom));
        assert!(matches!(infer_panel_edge(100, 400, 24, 24, monitor), PanelEdge::Left));
        assert!(matches!(infer_panel_edge(1_676, 400, 24, 24, monitor), PanelEdge::Right));
    }

    #[test]
    fn display_names_are_trimmed_limited_and_may_be_reset() {
        assert_eq!(normalized_display_name("  Work  ").as_deref(), Some("Work"));
        assert_eq!(normalized_display_name("  "), None);
        assert_eq!(normalized_display_name(&"x".repeat(80)).unwrap().chars().count(), 64);
    }

    #[test]
    fn removed_white_color_migrates_to_gray() {
        assert_eq!(normalized_account_color(Some("white"), "cyan"), "gray");
        assert_eq!(normalized_account_color(Some("#12abcf"), "cyan"), "#12abcf");
        assert_eq!(normalized_account_color(Some("invalid"), "cyan"), "cyan");
    }

    #[test]
    fn cached_identity_migrates_preferences_to_the_current_home() {
        let root = std::env::temp_dir().join(format!("agents-usage-identity-{}", uuid::Uuid::new_v4()));
        let old_home = root.join("old-shadow-home");
        let current_home = root.join("current-shadow-home");
        fs::create_dir_all(&current_home).unwrap();
        let email = "same@example.com";
        let snapshot = UsageSnapshot {
            email: Some(email.into()),
            bucket_name: Some("codex".into()),
            windows: Vec::new(),
            reset_available_count: 0,
            reset_credits: Vec::new(),
        };
        let mut config = AppConfig {
            accounts: vec![
                AccountPreference {
                    home: old_home.clone(),
                    display_name: Some("My account".into()),
                    ..AccountPreference::default()
                },
                AccountPreference {
                    home: current_home.clone(),
                    ..AccountPreference::default()
                },
            ],
            ..AppConfig::default()
        };
        let (cache, changed) = reconcile_cached_accounts(
            &mut config,
            vec![
                CachedUsage { provider_id: "openai".into(), home: old_home, snapshot: snapshot.clone() },
                CachedUsage { provider_id: "openai".into(), home: current_home.clone(), snapshot },
            ],
        );

        assert!(changed);
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].home, current_home);
        assert_eq!(config.accounts[0].display_name.as_deref(), Some("My account"));
        assert_eq!(config.accounts[0].identity_email.as_deref(), Some(email));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].home, config.accounts[0].home);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_move_targets_stop_at_list_boundaries() {
        assert_eq!(move_target(2, 4, -1), Some(1));
        assert_eq!(move_target(2, 4, 1), Some(3));
        assert_eq!(move_target(0, 4, -1), None);
        assert_eq!(move_target(3, 4, 1), None);
        assert_eq!(move_target(1, 4, 0), None);
    }

    #[test]
    fn account_move_keeps_config_and_dashboard_order_in_sync() {
        let root = std::env::temp_dir().join(format!("agents-usage-order-{}", uuid::Uuid::new_v4()));
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("auth.json"), b"{}").unwrap();
        fs::write(second.join("auth.json"), b"{}").unwrap();
        let mut config = AppConfig {
            accounts: vec![
                AccountPreference { home: first.clone(), ..AccountPreference::default() },
                AccountPreference { home: second.clone(), ..AccountPreference::default() },
            ],
            ..AppConfig::default()
        };
        let mut records = config.accounts.iter().enumerate()
            .filter_map(|(index, preference)| placeholder_record(preference, index, None))
            .collect::<Vec<_>>();
        let second_id = records[1].id.clone();

        assert!(move_account(&mut records, &mut config, &second_id, -1));
        assert_eq!(records[0].home, second);
        assert_eq!(config.accounts[0].home, second);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn placeholder_restores_only_saved_expansion() {
        let root = std::env::temp_dir().join(format!("agents-usage-expanded-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("auth.json"), b"{}").unwrap();
        let collapsed = AccountPreference {
            home: root.clone(),
            ..AccountPreference::default()
        };
        let expanded = AccountPreference {
            expanded: true,
            ..collapsed.clone()
        };

        assert!(!placeholder_record(&collapsed, 0, None).unwrap().expanded);
        assert!(placeholder_record(&expanded, 0, None).unwrap().expanded);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn placeholder_restores_cached_usage_before_refresh() {
        let root = std::env::temp_dir().join(format!("agents-usage-cache-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("auth.json"), b"{}").unwrap();
        let preference = AccountPreference { home: root.clone(), ..AccountPreference::default() };
        let snapshot = UsageSnapshot {
            email: Some("cached@example.com".into()),
            bucket_name: Some("codex".into()),
            windows: Vec::new(),
            reset_available_count: 0,
            reset_credits: Vec::new(),
        };

        let record = placeholder_record(&preference, 0, Some(snapshot)).unwrap();
        assert_eq!(record.email(), "cached@example.com");

        fs::remove_dir_all(root).unwrap();
    }
}
