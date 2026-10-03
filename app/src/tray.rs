//! Значок в трее (рядом с часами): открыть окно, выключить микрофон или звук,
//! выйти из программы. Пока он есть, крестик окна сворачивает программу
//! в трей, а не закрывает её (если это не выключено в настройках).

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

/// Пункты меню, которые меняются вместе с состоянием программы.
pub struct TrayItems {
    mute: CheckMenuItem<Wry>,
    deafen: CheckMenuItem<Wry>,
}

impl TrayItems {
    /// Показывает в меню, выключены ли микрофон и звук.
    pub fn show_status(&self, muted: bool, deafened: bool) {
        let _ = self.mute.set_checked(muted);
        let _ = self.deafen.set_checked(deafened);
    }
}

/// Создаёт значок. Ошибка не страшна (например, в Linux без трея):
/// программа просто будет закрываться крестиком, как раньше.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Открыть", true, None::<&str>)?;
    let mute =
        CheckMenuItem::with_id(app, "mute", "Выключить микрофон", true, false, None::<&str>)?;
    let deafen =
        CheckMenuItem::with_id(app, "deafen", "Выключить звук", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &mute,
            &deafen,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Поигрулькины")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_window(app),
            "mute" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let muted = !app.state::<crate::AppState>().tx.muted();
                    crate::apply_muted(&app, muted).await;
                });
            }
            "deafen" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<crate::AppState>();
                    let deafened = !state.deafened.load(std::sync::atomic::Ordering::Relaxed);
                    crate::apply_deafened(&app, deafened).await;
                });
            }
            "quit" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    crate::shutdown(&app).await;
                    app.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    app.manage(TrayItems { mute, deafen });
    Ok(())
}

/// Показывает главное окно (из трея или при повторном запуске программы).
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
