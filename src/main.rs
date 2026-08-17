#![allow(clippy::collapsible_if)] // Nested UI/state guards stay clearer than chained lock patterns.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod codex;
mod config;
mod discovery;
mod domain;
mod ui_model;

use crate::config::{AppConfig, AccountPreference};
use crate::domain::{AccountRecord, PendingReset};
use slint::{CloseRequestResponse, ComponentHandle};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

slint::include_modules!();

#[cfg(target_os = "linux")]
const DBUS_NAME: &str = "io.github.agentsusagetray.App";
#[cfg(target_os = "linux")]
const DBUS_PATH: &str = "/io/github/agentsusagetray/App";
const PANEL_GAP_PX: i32 = 6;
const SCREEN_MARGIN_PX: i32 = 5;
const OPEN_REFRESH_FRESHNESS: Duration = Duration::from_secs(5);
const STARTUP_REFRESH_DELAY: Duration = Duration::from_secs(10);
const MAX_RPC_CONCURRENCY: usize = 4;
const INTERACTIVE_REFRESH_CONCURRENCY: usize = 4;
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
    RefreshAtStartup,
    PersistSettings,
    ConsumeReset { account_id: String, credit_id: String },
    HidePanel,
    Tick,
    Quit,
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
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
    desktop.contains("kde") || desktop.contains("plasma")
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
    connection.get_input_focus().ok().and_then(|cookie| cookie.reply().ok()).map(|reply| reply.focus == xid).unwrap_or(false)
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
    let (model, enabled_count) = ui_model::model(&records, cfg.pin_short_global);
    let dashboard_height = ui_model::panel_height(&records, cfg.pin_short_global);
    let height = if ui.get_settings_visible() {
        ui.get_settings_height_px()
    } else {
        dashboard_height
    };
    ui.set_dashboard_height_px(dashboard_height);
    ui.set_accounts(model);
    ui.set_enabled_account_count(enabled_count as i32);
    ui.set_blur_emails(cfg.blur_emails);
    ui.set_pin_short_global(cfg.pin_short_global);
    ui.set_accounts_summary(format!("{} discovered · refresh also checks for new Codex homes", records.len()).into());
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

fn source_account_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("OpenAI")
        .to_string()
}

fn default_account_name(record: &AccountRecord) -> String {
    record
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.email.as_deref())
        .and_then(|email| email.split('@').next())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| source_account_name(&record.home))
}

fn normalized_display_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.chars().take(64).collect())
    }
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
    let pref_index = config.accounts.iter().position(|pref| canonical_id(&pref.home) == canonical_id(&home));
    let target_pref_index = config.accounts.iter().position(|pref| canonical_id(&pref.home) == canonical_id(&target_home));
    let (Some(pref_index), Some(target_pref_index)) = (pref_index, target_pref_index) else { return false; };
    records.swap(index, target);
    config.accounts.swap(pref_index, target_pref_index);
    true
}

fn record_from_snapshot(
    home: PathBuf,
    snapshot: crate::domain::UsageSnapshot,
    config: &mut AppConfig,
    color_index: usize,
) -> AccountRecord {
    let id = canonical_id(&home);
    let discovered_name = snapshot
        .email
        .as_deref()
        .and_then(|email| email.split('@').next())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| source_account_name(&home));
    let expand_globally = config.pin_short_global;
    let pref = config::preference_for_mut(config, &home);
    if pref.color.is_none() { pref.color = Some(ui_model::ACCOUNT_COLORS[color_index % ui_model::ACCOUNT_COLORS.len()].into()); }
    if pref.display_name.is_none() { pref.display_name = Some(discovered_name.clone()); }
    if expand_globally { pref.expanded = true; }
    AccountRecord {
        id,
        home,
        provider_id: "openai".into(),
        display_name: pref.display_name.clone().unwrap_or(discovered_name),
        color_name: pref.color.clone().unwrap_or_else(|| "cyan".into()),
        enabled: pref.enabled,
        pin_short: pref.pin_short,
        expanded: pref.expanded,
        email_revealed: false,
        confirm_credit_id: String::new(),
        snapshot: Some(snapshot),
        last_error: None,
    }
}

fn placeholder_record(
    pref: &AccountPreference,
    index: usize,
    expand_globally: bool,
) -> Option<AccountRecord> {
    if !pref.home.is_dir() { return None; }
    Some(AccountRecord {
        id: canonical_id(&pref.home),
        home: pref.home.clone(),
        provider_id: "openai".into(),
        display_name: pref.display_name.clone().unwrap_or_else(|| source_account_name(&pref.home)),
        color_name: pref.color.clone().unwrap_or_else(|| ui_model::ACCOUNT_COLORS[index % ui_model::ACCOUNT_COLORS.len()].into()),
        enabled: pref.enabled,
        pin_short: pref.pin_short,
        expanded: pref.expanded || expand_globally,
        email_revealed: false,
        confirm_credit_id: String::new(),
        snapshot: None,
        last_error: None,
    })
}

fn apply_snapshot(
    home: PathBuf,
    snapshot: crate::domain::UsageSnapshot,
    accounts: &Arc<Mutex<Vec<AccountRecord>>>,
    config: &Arc<Mutex<AppConfig>>,
) {
    let id = canonical_id(&home);
    let color_index = color_index_for_home(&home);
    let mut new_record = {
        let mut cfg = match config.lock() {
            Ok(cfg) => cfg,
            Err(_) => return,
        };
        let previous = cfg.clone();
        let record = record_from_snapshot(home, snapshot, &mut cfg, color_index);
        if *cfg != previous {
            if let Err(error) = config::save(&cfg) {
                eprintln!("settings: could not persist account discovery: {error}");
            }
        }
        record
    };

    if let Ok(mut records) = accounts.lock() {
        if let Some(existing) = records.iter_mut().find(|record| record.id == id) {
            new_record.expanded = existing.expanded;
            new_record.email_revealed = existing.email_revealed;
            new_record.confirm_credit_id = existing.confirm_credit_id.clone();
            *existing = new_record;
        } else {
            records.push(new_record);
        }
        let order = config
            .lock()
            .map(|cfg| {
                cfg.accounts
                    .iter()
                    .enumerate()
                    .map(|(index, pref)| (canonical_id(&pref.home), index))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        records.sort_by_key(|record| order.get(&record.id).copied().unwrap_or(usize::MAX));
    }
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
    codex_path: PathBuf,
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
            records
                .iter()
                .filter(|record| record.enabled)
                .map(|record| (record.id.clone(), record.home.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if targets.is_empty() {
        return;
    }

    let mut jobs = JoinSet::new();
    let operation_semaphore = Arc::new(Semaphore::new(operation_concurrency.max(1)));
    for (id, home) in targets {
        let codex_path = codex_path.clone();
        let rpc_semaphore = rpc_semaphore.clone();
        let operation_semaphore = operation_semaphore.clone();
        jobs.spawn(async move {
            let Ok(_operation_permit) = operation_semaphore.acquire_owned().await else { return None; };
            let Ok(_permit) = rpc_semaphore.acquire_owned().await else { return None; };
            let result = codex::read_openai_account(&codex_path, &home).await;
            Some((id, home, result))
        });
    }

    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(Some((_id, home, Ok(snapshot)))) => {
                apply_snapshot(home, snapshot, &accounts, &config);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
            Ok(Some((id, _home, Err(error)))) => {
                eprintln!("refresh: {id}: {error}");
                let user_message = error.user_message().to_string();
                apply_refresh_error(&id, user_message, &accounts);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
            Ok(None) => {}
            Err(error) => eprintln!("refresh task failed: {error}"),
        }
    }
}

async fn discover_new_accounts(
    codex_path: PathBuf,
    accounts: Arc<Mutex<Vec<AccountRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    ui: slint::Weak<MainWindow>,
    last_anchor: Arc<Mutex<Option<PanelAnchor>>>,
    rpc_semaphore: Arc<Semaphore>,
    operation_concurrency: usize,
) {
    let cfg = config.lock().map(|cfg| cfg.clone()).unwrap_or_default();
    let candidates = discovery::candidate_codex_homes(&cfg);
    let known = accounts
        .lock()
        .map(|records| records.iter().map(|record| record.id.clone()).collect::<HashSet<_>>())
        .unwrap_or_default();
    let unknown = candidates
        .into_iter()
        .filter(|home| !known.contains(&canonical_id(home)))
        .collect::<Vec<_>>();

    if unknown.is_empty() {
        return;
    }

    eprintln!("discovery: probing {} new Codex-home candidate(s)", unknown.len());
    let mut jobs = JoinSet::new();
    let operation_semaphore = Arc::new(Semaphore::new(operation_concurrency.max(1)));
    for (order, home) in unknown.into_iter().enumerate() {
        let codex_path = codex_path.clone();
        let rpc_semaphore = rpc_semaphore.clone();
        let operation_semaphore = operation_semaphore.clone();
        jobs.spawn(async move {
            let Ok(_operation_permit) = operation_semaphore.acquire_owned().await else { return None; };
            let Ok(_permit) = rpc_semaphore.acquire_owned().await else { return None; };
            let result = codex::read_openai_account(&codex_path, &home).await;
            Some((order, home, result))
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
    completed.sort_by_key(|(order, _, _)| *order);
    for (_, home, result) in completed {
        match result {
            Ok(snapshot) => {
                eprintln!("discovery: found OpenAI account at {}", home.display());
                apply_snapshot(home, snapshot, &accounts, &config);
                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor.clone(), None);
            }
            Err(error) => {
                // A shallow candidate may be an incomplete/old Codex directory. It is
                // intentionally not persisted as an account unless Codex validates it.
                eprintln!("discovery: ignoring {}: {error}", home.display());
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
                    apply_snapshot(home, snapshot, &accounts, &config);
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
                    apply_snapshot(pending.codex_home, snapshot, &accounts, &config);
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
                        schedule_render(
                            ui.clone(), accounts.clone(), config.clone(), last_anchor_shared.clone(),
                            Some("Codex was not found. Install Codex or set AGENTS_USAGE_CODEX_BIN to its executable.".into()),
                        );
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
                            eprintln!("KDE StatusNotifierItem ready");
                            Some(handle)
                        }
                        Err(error) => {
                            eprintln!("KDE StatusNotifierItem unavailable: {error}");
                            None
                        }
                    }
                } else {
                    None
                };

                schedule_render(ui.clone(), accounts.clone(), config.clone(), last_anchor_shared.clone(), None);

                match (codex_path.clone(), config::load_pending_reset()) {
                    (Some(codex_path), Ok(Some(pending))) => {
                        tokio::spawn(reconcile_pending_reset(
                            codex_path,
                            pending,
                            accounts.clone(),
                            config.clone(),
                            ui.clone(),
                            last_anchor_shared.clone(),
                        ));
                    }
                    (_, Err(error)) => eprintln!(
                        "reset: pending transaction journal is unreadable; reset use remains blocked: {error}"
                    ),
                    _ => {}
                }

                // Background/autostart launches yield to the desktop login workload.
                // If the user opens the panel first, RefreshIfStale runs immediately and
                // this delayed pass becomes a no-op because the data is already fresh.
                let startup_refresh_tx = tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(STARTUP_REFRESH_DELAY).await;
                    send(&startup_refresh_tx, WorkerCommand::RefreshAtStartup);
                });

                let tick_tx = tx.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        if tick_tx.send(WorkerCommand::Tick).is_err() { break; }
                    }
                });

                let refreshing = Arc::new(AtomicBool::new(false));
                let refresh_pending = Arc::new(AtomicBool::new(false));
                let discovering = Arc::new(AtomicBool::new(false));
                let rpc_semaphore = Arc::new(Semaphore::new(MAX_RPC_CONCURRENCY));
                let last_data_refresh = Arc::new(Mutex::new(None::<Instant>));

                while let Some(command) = rx.recv().await {
                    match command {
                        #[cfg(target_os = "linux")]
                        WorkerCommand::ToggleAt(anchor) => {
                            if let Ok(mut shared) = last_anchor_shared.lock() { *shared = Some(anchor); }
                            let opening = !panel_visible.load(Ordering::SeqCst);
                            panel_visible.store(opening, Ordering::SeqCst);
                            if opening {
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
                            schedule_show_dashboard(
                                ui.clone(), Some(anchor), native_xid_shared.clone(), panel_visible.clone(),
                            );
                            send(&tx, WorkerCommand::RefreshIfStale);
                        }
                        #[cfg(target_os = "linux")]
                        WorkerCommand::OpenSettingsAt(anchor) => {
                            if let Ok(mut shared) = last_anchor_shared.lock() { *shared = Some(anchor); }
                            panel_visible.store(true, Ordering::SeqCst);
                            schedule_show_settings(
                                ui.clone(), Some(anchor), native_xid_shared.clone(), panel_visible.clone(),
                            );
                            send(&tx, WorkerCommand::RefreshIfStale);
                        }
                        WorkerCommand::OpenSettings => {
                            panel_visible.store(true, Ordering::SeqCst);
                            let anchor = last_anchor_shared.lock().ok().and_then(|value| *value);
                            schedule_show_settings(
                                ui.clone(), anchor, native_xid_shared.clone(), panel_visible.clone(),
                            );
                        }
                        WorkerCommand::OpenStandalone => {
                            panel_visible.store(true, Ordering::SeqCst);
                            let anchor = last_anchor_shared.lock().ok().and_then(|value| *value);
                            schedule_show_dashboard(
                                ui.clone(), anchor, native_xid_shared.clone(), panel_visible.clone(),
                            );
                            send(&tx, WorkerCommand::RefreshIfStale);
                        }
                        WorkerCommand::ToggleAtPoint { x, y, icon_w, icon_h } => {
                            let opening = !panel_visible.load(Ordering::SeqCst);
                            panel_visible.store(opening, Ordering::SeqCst);
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
                        refresh_command @ (WorkerCommand::Refresh | WorkerCommand::RefreshIfStale | WorkerCommand::RefreshAtStartup) => {
                            let force = matches!(refresh_command, WorkerCommand::Refresh);
                            let startup_pass = matches!(refresh_command, WorkerCommand::RefreshAtStartup);
                            if !force {
                                let still_fresh = last_data_refresh
                                    .lock()
                                    .ok()
                                    .and_then(|value| *value)
                                .map(|when| {
                                    when.elapsed() < if startup_pass {
                                        Duration::from_secs(60)
                                    } else {
                                        OPEN_REFRESH_FRESHNESS
                                    }
                                })
                                    .unwrap_or(false);
                                if still_fresh {
                                    eprintln!("refresh: open skipped because account data is less than {}s old", OPEN_REFRESH_FRESHNESS.as_secs());
                                    continue;
                                }
                            }
                            let Some(codex_path) = codex_path.clone() else { continue; };
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
                            tokio::spawn(async move {
                                let had_known_accounts = accounts2
                                    .lock()
                                    .map(|records| records.iter().any(|record| record.enabled))
                                    .unwrap_or(false);

                                // Fast lane: refresh already-known enabled accounts first.
                                refresh_known_accounts(
                                    codex_path.clone(), accounts2.clone(), config2.clone(), ui2.clone(), anchor2.clone(),
                                    rpc_semaphore2.clone(),
                                    if startup_pass { 1 } else { INTERACTIVE_REFRESH_CONCURRENCY },
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
                                        codex_path, accounts2, config2, ui2.clone(), anchor2,
                                        rpc_semaphore2,
                                        if startup_pass { 1 } else { INTERACTIVE_DISCOVERY_CONCURRENCY },
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
                            let _ = ui.upgrade_in_event_loop(|ui| { let _ = ui.hide(); });
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

    let open_on_start = launch_mode(std::env::args_os().skip(1)) == LaunchMode::Open;
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

    let loaded_config = config::load();
    let expand_globally = loaded_config.pin_short_global;
    let initial_records = loaded_config
        .accounts
        .iter()
        .enumerate()
        .filter_map(|(index, pref)| placeholder_record(pref, index, expand_globally))
        .collect::<Vec<_>>();

    let accounts = Arc::new(Mutex::new(initial_records));
    let config = Arc::new(Mutex::new(loaded_config));
    let last_anchor_shared = Arc::new(Mutex::new(None::<PanelAnchor>));
    let native_xid_shared = Arc::new(Mutex::new(None::<u32>));
    let panel_visible = Arc::new(AtomicBool::new(false));
    let (tx, rx) = unbounded_channel::<WorkerCommand>();

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let _native_tray = match create_native_tray(tx.clone()) {
        Ok(tray) => Some(tray),
        Err(error) => {
            eprintln!("native tray creation failed: {error}");
            send(&tx, WorkerCommand::OpenStandalone);
            None
        }
    };

    render_ui(&ui, &accounts, &config, &last_anchor_shared, Some("Looking for Codex accounts…"));

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
                    Some((record.home.clone(), record.expanded))
                } else {
                    None
                }
            } else { None };
            if let Some((home, expanded)) = changed {
                if let Ok(mut cfg) = config.lock() {
                    config::preference_for_mut(&mut cfg, &home).expanded = expanded;
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
        ui.on_pin_short_global_changed(move |value| {
            if value {
                if let Ok(mut records) = accounts.lock() {
                    for record in records.iter_mut() { record.expanded = true; }
                }
            }
            if let Ok(mut cfg) = config_arc.lock() {
                cfg.pin_short_global = value;
                if value {
                    for preference in &mut cfg.accounts { preference.expanded = true; }
                }
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
                    record.home.clone()
                })
            } else { None };
            if let Some(home) = home {
                if let Ok(mut cfg) = config_arc.lock() { config::preference_for_mut(&mut cfg, &home).enabled = value; }
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
                    record.home.clone()
                })
            } else { None };
            if let Some(home) = home {
                if let Ok(mut cfg) = config_arc.lock() { config::preference_for_mut(&mut cfg, &home).pin_short = value; }
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
                    record.home.clone()
                })
            } else { None };
            if let Some(home) = home {
                if let Ok(mut cfg) = config_arc.lock() {
                    config::preference_for_mut(&mut cfg, &home).display_name = requested_name;
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
                    record.home.clone()
                })
            } else { None };
            if let Some(home) = home {
                if let Ok(mut cfg) = config_arc.lock() {
                    config::preference_for_mut(&mut cfg, &home).color = Some(color.to_string());
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
        #[cfg(target_os = "linux")]
        let native_xid_shared = native_xid_shared.clone();
        ui.window().on_winit_window_event(move |_slint_window, event| {
            match event {
                winit::event::WindowEvent::Focused(focused) => {
                    #[cfg(target_os = "linux")]
                    let xid = native_xid_shared.lock().ok().and_then(|guard| *guard);
                    #[cfg(target_os = "linux")]
                    let has_x11_focus = xid.map(x11_popup_has_input_focus).unwrap_or(false);
                    #[cfg(target_os = "linux")]
                    if !*focused && xid.is_some() && !has_x11_focus {
                        send(&tx, WorkerCommand::HidePanel);
                    }
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
        LaunchMode, PanelAnchor, PanelEdge, SCREEN_MARGIN_PX, launch_mode, move_account,
        infer_panel_edge, move_target, normalized_display_name, panel_position_for_size,
        placeholder_record,
    };
    use crate::config::{AccountPreference, AppConfig};
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
        let mut config = AppConfig {
            accounts: vec![
                AccountPreference { home: first.clone(), ..AccountPreference::default() },
                AccountPreference { home: second.clone(), ..AccountPreference::default() },
            ],
            ..AppConfig::default()
        };
        let mut records = config.accounts.iter().enumerate()
            .filter_map(|(index, preference)| placeholder_record(preference, index, false))
            .collect::<Vec<_>>();
        let second_id = records[1].id.clone();

        assert!(move_account(&mut records, &mut config, &second_id, -1));
        assert_eq!(records[0].home, second);
        assert_eq!(config.accounts[0].home, second);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn placeholder_restores_saved_or_global_expansion() {
        let root = std::env::temp_dir().join(format!("agents-usage-expanded-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let collapsed = AccountPreference {
            home: root.clone(),
            ..AccountPreference::default()
        };
        let expanded = AccountPreference {
            expanded: true,
            ..collapsed.clone()
        };

        assert!(!placeholder_record(&collapsed, 0, false).unwrap().expanded);
        assert!(placeholder_record(&expanded, 0, false).unwrap().expanded);
        assert!(placeholder_record(&collapsed, 0, true).unwrap().expanded);

        fs::remove_dir_all(root).unwrap();
    }
}
