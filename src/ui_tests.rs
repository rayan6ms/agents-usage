//! Render the real UI without creating a native window, using D-Bus, or reading
//! user accounts. Optional PPM screenshots go to AGENTS_USAGE_UI_TEST_OUTPUT.
use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use std::rc::Rc;

struct HeadlessPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn snapshot(ui: &MainWindow, window: &MinimalSoftwareWindow, label: &str, scale: f32) {
    window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: scale });
    // Simulate the native resize requested by show_settings/show_dashboard.
    window.set_size(slint::LogicalSize::new(360.0, ui.get_desired_height_px()));
    let pixels = ui.window().take_snapshot().expect("render headless UI");
    assert_eq!(pixels.width(), (360.0 * scale).round() as u32);
    assert_eq!(pixels.height(), (ui.get_desired_height_px() * scale).round() as u32);
    if let Some(directory) = std::env::var_os("AGENTS_USAGE_UI_TEST_OUTPUT") {
        use std::io::Write;
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let mut file = std::fs::File::create(directory.join(format!("{label}-{scale}.ppm"))).unwrap();
        writeln!(file, "P6\n{} {}\n255", pixels.width(), pixels.height()).unwrap();
        let rgb: Vec<u8> = pixels.as_slice().iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        file.write_all(&rgb).unwrap();
    }
}

#[test]
fn settings_grows_from_one_account_and_back_restores_dashboard() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform(window.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    ui.set_motion_enabled(false);
    let records = Arc::new(Mutex::new(vec![AccountRecord {
        id: "test-account".into(), home: PathBuf::new(), provider_id: "openai".into(),
        display_name: "Personal".into(), color_name: "cyan".into(), enabled: true,
        pin_short: false, expanded: false, name_revealed: false, email_revealed: false,
        confirm_credit_id: String::new(), last_error: None,
        snapshot: Some(UsageSnapshot {
            email: Some("personal@example.com".into()), plan_type: Some("plus".into()),
            bucket_name: None, windows: Vec::new(), reset_available_count: 0, reset_credits: Vec::new(),
        }),
    }]));
    let config = Arc::new(Mutex::new(AppConfig::default()));
    let anchor = Arc::new(Mutex::new(None));
    let native_xid = Arc::new(Mutex::new(None));
    render_ui(&ui, &records, &config, &anchor, None);
    let compact_height = ui.get_dashboard_height_px();
    assert!(compact_height < 520.0);
    show_dashboard(&ui, None, &native_xid);
    snapshot(&ui, &window, "dashboard", 1.0);

    for scale in [1.0, 1.5, 2.0] {
        show_settings(&ui, None, &native_xid);
        assert!(ui.get_settings_visible());
        assert_eq!(ui.get_settings_height_px(), 520.0);
        assert_eq!(ui.get_desired_height_px(), 520.0);
        render_ui(&ui, &records, &config, &anchor, None);
        assert_eq!(ui.get_desired_height_px(), 520.0, "refresh must not shrink settings");
        snapshot(&ui, &window, "settings", scale);
        show_dashboard(&ui, None, &native_xid);
        assert!(!ui.get_settings_visible());
        assert_eq!(ui.get_desired_height_px(), compact_height);
    }
    snapshot(&ui, &window, "dashboard-back", 1.0);
    records.lock().unwrap()[0].enabled = false;
    render_ui(&ui, &records, &config, &anchor, None);
    show_settings(&ui, None, &native_xid);
    assert_eq!(ui.get_desired_height_px(), 520.0);
    snapshot(&ui, &window, "settings-disabled", 1.0);
}
