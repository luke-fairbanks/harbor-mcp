//! Harbor — Tauri entry point. Wires the config store, supervisor, shared state,
//! and the in-process MCP server together (DESIGN.md §3).

mod commands;
mod detect;
mod discovery;
mod health;
mod mcp;
mod model;
mod ops;
mod ports;
mod state;
mod store;
mod supervisor;
mod sysenv;
mod tray;

use state::AppState;
use std::sync::Arc;
use store::{McpSettings, Store};
use supervisor::Supervisor;
use tauri::Manager;

/// Default preferred port for the MCP server (DESIGN.md §3.2).
const DEFAULT_MCP_PORT: u16 = 7777;

fn new_token() -> String {
    // 64 hex chars (~244 bits) — ample for a localhost bearer token.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // This must be the first plugin registered so duplicate launches are
        // intercepted before any other plugin initialization can run.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // Remember window size/position across launches. The tray popover is
        // excluded so it always starts hidden (its visibility is driven by the
        // tray icon, not restored state).
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_denylist(&[tray::TRAY_LABEL])
                .build(),
        )
        .setup(|app| {
            let handle = app.handle().clone();

            // --- app data dir + config store ---
            let data_dir = app.path().app_data_dir().expect("resolve app data dir");
            // Shared with the supervisor, which persists/reads `runs.json` to
            // re-adopt servers left running by a previous session.
            let store = Arc::new(Store::new(&data_dir));

            // --- registry: an empty first run enters folder onboarding ---
            let registry = store.load_registry()?.apps;

            // Keep the preferred port, but rotate the bearer token every launch.
            // Restart-safe clients read mcp.json through the bridge; a token
            // observed from a stale listener while Harbor is down is therefore
            // invalid by the time Harbor serves requests again.
            let preferred_port = store
                .load_settings()
                .ok()
                .flatten()
                .map(|settings| settings.port)
                .unwrap_or(DEFAULT_MCP_PORT);
            let mut mcp = McpSettings {
                token: new_token(),
                port: preferred_port,
            };
            let (live_port, mcp_listener) = mcp::bind_listener(mcp.port)?;
            if live_port != mcp.port {
                mcp.port = live_port;
            }
            store.save_settings(&mcp)?;
            eprintln!(
                "[harbor] MCP server → http://127.0.0.1:{}/mcp (token {}…)",
                mcp.port,
                &mcp.token[..8.min(mcp.token.len())]
            );

            // --- shared state, reachable from commands AND the MCP server ---
            // The registry is shared (Arc) with the supervisor so auto-restart
            // reads live, possibly-edited config at crash time.
            let registry = Arc::new(tokio::sync::RwLock::new(registry));
            let supervisor = Supervisor::new(handle.clone(), store.clone(), registry.clone());
            let app_state = Arc::new(AppState::new(
                store.clone(),
                registry,
                supervisor,
                mcp.clone(),
            ));
            app.manage(app_state.clone());

            // Ask for notification permission up front (non-blocking) so crash
            // alerts can fire later. Best-effort; denial degrades to in-app only.
            {
                use tauri_plugin_notification::{NotificationExt, PermissionState};
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let granted = h
                        .notification()
                        .permission_state()
                        .map(|s| s == PermissionState::Granted)
                        .unwrap_or(false);
                    if !granted {
                        let _ = h.notification().request_permission();
                    }
                });
            }

            // Re-adopt any servers a previous session left running, before the
            // window paints — so the UI shows them as running and a duplicate
            // Start is short-circuited instead of crashing on EADDRINUSE.
            {
                let st = app_state.clone();
                tauri::async_runtime::block_on(async move {
                    // 1. Re-adopt servers this Harbor spawned in a prior session.
                    st.supervisor.adopt_persisted().await;
                    // 2. Reflect servers started OUTSIDE Harbor (e.g. a terminal)
                    //    that corroborate as a registered app, so they show as
                    //    running immediately.
                    let configs = st.list_configs().await;
                    st.supervisor.scan_and_adopt_external(&configs).await;
                });
            }

            // Per-service resource sampler (group CPU% + RSS, ~2s). After adoption
            // so adopted/external services are sampled too.
            app_state.supervisor.spawn_sampler();

            // --- host the MCP server on Tauri's tokio runtime ---
            let server_state = app_state.clone();
            let token = mcp.token.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = mcp::serve_on(server_state, token, mcp_listener).await {
                    eprintln!("[harbor] MCP server stopped: {e}");
                }
            });

            // Menu-bar tray icon + popover panel.
            if let Err(e) = tray::setup(&handle) {
                eprintln!("[harbor] tray setup failed: {e}");
            }

            // Window is created hidden (config `visible: false`) to avoid the
            // transparent-window white flash; reveal once setup is done.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_apps,
            commands::app_status,
            commands::list_local_servers,
            commands::stop_local_server,
            commands::start_app,
            commands::stop_app,
            commands::get_logs,
            commands::register_app,
            commands::approve_app,
            commands::update_app,
            commands::remove_app,
            commands::set_env,
            commands::set_port,
            commands::detect_app,
            commands::open_app,
            commands::open_url,
            commands::mcp_info,
            commands::import_app,
            commands::export_app,
            commands::show_main_window,
            commands::agents_status,
            commands::connect_claude_code,
            commands::connect_claude_desktop,
            commands::connect_codex,
            commands::fix_prompt,
            commands::run_fix,
            commands::restart_app,
            commands::start_all,
            commands::stop_all,
            commands::path_kind,
            commands::read_dotenv,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // On quit, flag the supervisor as shutting down so any in-flight
            // auto-restart backoff / crash notification no-ops. We deliberately
            // do NOT stop the servers — Harbor-spawned servers survive a Harbor
            // restart and are re-adopted next launch (see adopt_persisted).
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(state) = app_handle.try_state::<Arc<AppState>>() {
                    state.supervisor.begin_shutdown();
                }
            }
        });
}
