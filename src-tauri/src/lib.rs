//! Harbor — Tauri entry point. Wires the config store, supervisor, shared state,
//! and the in-process MCP server together (DESIGN.md §3).

mod commands;
mod detect;
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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("resolve app data dir");
            let store = Store::new(&data_dir);

            // --- registry: load, or seed QuizletLocal on first run ---
            let mut registry = store.load_registry().unwrap_or_default().apps;
            if registry.is_empty() {
                if let Ok(home) = std::env::var("HOME") {
                    let ql = std::path::Path::new(&home).join("Desktop/QuizletLocal");
                    if ql.exists() {
                        let cfg = store::quizletlocal_seed(ql.to_string_lossy().into_owned());
                        registry.insert(cfg.name.clone(), cfg);
                        let snapshot = store::Registry {
                            apps: registry.clone(),
                        };
                        let _ = store.save_registry(&snapshot);
                        eprintln!("[harbor] seeded QuizletLocal from {}", ql.display());
                    }
                }
            }

            // --- MCP settings: token (persisted) + a currently-bindable port ---
            let mut mcp = match store.load_settings() {
                Ok(Some(s)) => s,
                _ => McpSettings {
                    token: new_token(),
                    port: DEFAULT_MCP_PORT,
                },
            };
            let live_port = mcp::pick_free_port(mcp.port);
            if live_port != mcp.port {
                mcp.port = live_port;
            }
            let _ = store.save_settings(&mcp);
            eprintln!(
                "[harbor] MCP server → http://127.0.0.1:{}/mcp (token {}…)",
                mcp.port,
                &mcp.token[..8.min(mcp.token.len())]
            );

            // --- shared state, reachable from commands AND the MCP server ---
            let supervisor = Supervisor::new(handle.clone());
            let app_state = Arc::new(AppState::new(store, registry, supervisor, mcp.clone()));
            app.manage(app_state.clone());

            // --- host the MCP server on Tauri's tokio runtime ---
            let server_state = app_state.clone();
            let token = mcp.token.clone();
            let port = mcp.port;
            tauri::async_runtime::spawn(async move {
                if let Err(e) = mcp::serve(server_state, token, port).await {
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
            commands::start_app,
            commands::stop_app,
            commands::get_logs,
            commands::register_app,
            commands::update_app,
            commands::remove_app,
            commands::set_env,
            commands::set_port,
            commands::detect_app,
            commands::open_app,
            commands::mcp_info,
            commands::import_app,
            commands::export_app,
            commands::show_main_window,
            commands::claude_status,
            commands::connect_claude_code,
            commands::connect_claude_desktop,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
