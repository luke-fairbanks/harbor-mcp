//! Menu-bar tray icon + a popover panel window.
//!
//! Left-click the menu-bar anchor → a small always-on-top panel appears under it
//! listing your apps with start/stop/open controls; it hides when it loses focus.
//! Right-click → a native menu (Open Harbor / Quit). The panel is the same
//! frontend bundle rendered in a window labelled `tray` (the React side branches
//! on the window label).

use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, LogicalPosition, Manager, Rect, Runtime, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};

pub const TRAY_LABEL: &str = "tray";
const POPOVER_W: f64 = 320.0;
const POPOVER_H: f64 = 468.0;

/// Remembers when the popover was last hidden, so the tray click that *caused*
/// the blur-hide doesn't immediately re-open it.
#[derive(Default)]
pub struct TrayState {
    last_hidden: Mutex<Option<Instant>>,
}

pub fn setup<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    app.manage(TrayState::default());

    // Popover window — created hidden, shown on tray click.
    let popover = WebviewWindowBuilder::new(app, TRAY_LABEL, WebviewUrl::App("index.html".into()))
        .title("Harbor")
        .inner_size(POPOVER_W, POPOVER_H)
        .visible(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .resizable(false)
        .skip_taskbar(true)
        .shadow(true)
        .build()?;

    {
        let win = popover.clone();
        let handle = app.clone();
        popover.on_window_event(move |event| {
            if let WindowEvent::Focused(false) = event {
                let _ = win.hide();
                if let Some(state) = handle.try_state::<TrayState>() {
                    *state.last_hidden.lock().unwrap() = Some(Instant::now());
                }
            }
        });
    }

    // Right-click menu.
    let open_item = MenuItem::with_id(app, "open_harbor", "Open Harbor", true, None::<&str>)?;
    let quit_item = PredefinedMenuItem::quit(app, Some("Quit Harbor"))?;
    let menu = MenuBuilder::new(app)
        .item(&open_item)
        .separator()
        .item(&quit_item)
        .build()?;

    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;

    TrayIconBuilder::with_id("harbor-tray")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Harbor")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "open_harbor" {
                show_main(app);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), rect);
            }
        })
        .build(app)?;

    Ok(())
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_popover<R: Runtime>(app: &AppHandle<R>, rect: Rect) {
    let Some(win) = app.get_webview_window(TRAY_LABEL) else {
        return;
    };

    // Don't re-open if this same click just blurred-and-hid it.
    if let Some(state) = app.try_state::<TrayState>() {
        if let Some(t) = *state.last_hidden.lock().unwrap() {
            if t.elapsed() < Duration::from_millis(250) {
                return;
            }
        }
    }

    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }

    // Center the panel horizontally under the icon, just below the menu bar.
    let scale = win.scale_factor().unwrap_or(1.0);
    let pos = rect.position.to_logical::<f64>(scale);
    let size = rect.size.to_logical::<f64>(scale);
    let x = pos.x + size.width / 2.0 - POPOVER_W / 2.0;
    let y = pos.y + size.height + 2.0;
    let _ = win.set_position(LogicalPosition::new(x.max(8.0), y.max(8.0)));

    let _ = win.show();
    let _ = win.set_focus();
}
