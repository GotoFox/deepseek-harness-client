//! DSH Client — the Tauri 2 desktop shell for DeepSeek Harness (dsh).
//!
//! The shell bundles a Node.js runtime and a self-contained dsh kernel
//! tarball, spawns `dsh web` on a random loopback port, supervises the
//! process, and hosts the UI in the app window. The tray menu owns the
//! app-level lifecycle (show/hide, autostart, updates, quit).

mod kernel;

use std::thread;
use std::time::Duration;

use tauri::menu::{CheckMenuItemBuilder, Menu, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_updater::UpdaterExt;

use kernel::KernelManager;

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn quit_app(app: &AppHandle) {
    let km = app.state::<KernelManager>();
    km.stop();
    app.exit(0);
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "打开主界面").build(app)?;
    let check_client = MenuItemBuilder::with_id("check-client", "检查客户端更新").build(app)?;
    let check_kernel = MenuItemBuilder::with_id("check-kernel", "检查内核更新").build(app)?;
    let open_logs_item = MenuItemBuilder::with_id("open-logs", "打开日志目录").build(app)?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "开机自启").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出 DSH Client").build(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &check_client,
            &check_kernel,
            &open_logs_item,
            &PredefinedMenuItem::separator(app)?,
            &autostart,
            &quit,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().map(|i| i.clone()).expect("app icon"))
        .tooltip("DSH Client")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "check-client" => {
                tauri::async_runtime::spawn(check_updates(app.clone()));
            }
            "check-kernel" => {
                let km = app.state::<KernelManager>();
                km.check_kernel_update(app.clone());
                let _ = app.emit("kernel-update-progress", "正在检查内核更新…");
            }
            "open-logs" => {
                let _ = open_logs(app);
            }
            "autostart" => {
                let autostart = app.autolaunch();
                match autostart.is_enabled() {
                    Ok(true) => {
                        let _ = autostart.disable();
                    }
                    Ok(false) => {
                        let _ = autostart.enable();
                    }
                    Err(_) => {}
                }
            }
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    // Reflect the current autostart state on the menu checkbox.
    let enabled = app.autolaunch().is_enabled().unwrap_or(false);
    let _ = autostart.set_checked(enabled);
    // Keep the handle alive; Tauri also tracks it internally.
    let _ = tray;
    Ok(())
}

/// Check for a client (shell) update and, if one exists, download and install
/// it in the background. The app restarts after the install completes.
async fn check_updates(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    let Some(update) = update else {
        let _ = app.emit("update-result", "已是最新版本");
        return Ok(());
    };
    let _ = app.emit("update-available", update.version.to_string());
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("update-installed", ());
    Ok(())
}

fn open_logs(app: &AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(&dir);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(not(target_os = "windows"))]
    cmd.arg(&dir);
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_status(state: State<'_, KernelManager>) -> kernel::KernelStatus {
    let mut status = state.status();
    if cfg!(debug_assertions) && status.phase == "idle" {
        status.phase = "dev".to_string();
    }
    status
}

#[tauri::command]
fn retry_kernel(app: AppHandle, state: State<'_, KernelManager>) {
    state.retry(app);
}

#[tauri::command]
fn check_updates_cmd(app: AppHandle) -> String {
    tauri::async_runtime::spawn(check_updates(app));
    "checking".to_string()
}

#[tauri::command]
fn check_kernel_update_cmd(app: AppHandle, state: State<'_, KernelManager>) -> String {
    state.check_kernel_update(app);
    "checking".to_string()
}

#[tauri::command]
fn open_logs_cmd(app: AppHandle) -> Result<(), String> {
    open_logs(&app)
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(KernelManager::new())
        .setup(|app| {
            setup_tray(app.handle())?;

            // Closing the window hides to tray instead of quitting.
            if let Some(window) = app.get_webview_window("main") {
                let win = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win.hide();
                    }
                });
            }

            if !cfg!(debug_assertions) {
                let km = app.state::<KernelManager>();
                km.start(app.handle().clone());

                // Periodic background checks: client update + kernel update.
                let app1 = app.handle().clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_secs(10));
                    let _ = tauri::async_runtime::block_on(check_updates(app1.clone()));
                    let km = app1.state::<KernelManager>();
                    km.check_kernel_update(app1.clone());
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            retry_kernel,
            check_updates_cmd,
            check_kernel_update_cmd,
            open_logs_cmd,
            restart_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DSH Client");
}
