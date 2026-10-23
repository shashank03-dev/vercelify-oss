//! Menu-bar item: the selected server's state at a glance, and a way back
//! into the app. The UI sets the status line (`tray_status`) from the live
//! data it already has, so the tray makes no requests of its own.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};

pub struct Tray {
    status: MenuItem<Wry>,
    icon: TrayIcon<Wry>,
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn setup(app: &tauri::App) -> tauri::Result<Tray> {
    let status = MenuItem::with_id(app, "status", "Connecting…", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Vercelify", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Vercelify", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status, &PredefinedMenuItem::separator(app)?, &open, &quit])?;
    let mut builder = TrayIconBuilder::with_id("vercelify")
        .tooltip("Vercelify")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    let icon = builder.build(app)?;
    Ok(Tray { status, icon })
}

/// One short line, e.g. "thinkpad-t470 · all good".
pub fn status_line(text: &str) -> String {
    let t: String = text.chars().filter(|c| !c.is_control()).take(80).collect();
    if t.trim().is_empty() {
        "Vercelify".into()
    } else {
        t
    }
}

impl Tray {
    pub fn set_status(&self, text: &str) {
        let line = status_line(text);
        let _ = self.status.set_text(&line);
        let _ = self.icon.set_tooltip(Some(format!("Vercelify — {line}")));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_line_is_short_and_clean() {
        assert_eq!(super::status_line("pi5 · all good"), "pi5 · all good");
        assert_eq!(super::status_line("a\nb"), "ab");
        assert_eq!(super::status_line("  "), "Vercelify");
        assert_eq!(super::status_line(&"x".repeat(200)).len(), 80);
    }
}
