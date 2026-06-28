import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AppConfig,
  AppListItem,
  AppRunSnapshot,
  Detection,
  LogLine,
  McpInfo,
  StatusEvent,
} from "./types";

export const api = {
  listApps: () => invoke<AppListItem[]>("list_apps"),
  appStatus: (app: string) => invoke<AppRunSnapshot | null>("app_status", { app }),
  startApp: (app: string, profile?: string) =>
    invoke<AppRunSnapshot>("start_app", { app, profile }),
  stopApp: (app: string) => invoke<void>("stop_app", { app }),
  getLogs: (app: string, service: string, lines?: number) =>
    invoke<LogLine[]>("get_logs", { app, service, lines }),
  registerApp: (config: AppConfig) => invoke<void>("register_app", { config }),
  updateApp: (app: string, config: AppConfig) =>
    invoke<void>("update_app", { app, config }),
  removeApp: (app: string) => invoke<boolean>("remove_app", { app }),
  setEnv: (app: string, service: string, env: Record<string, string>) =>
    invoke<boolean>("set_env", { app, service, env }),
  setPort: (app: string, service: string, port: number) =>
    invoke<boolean>("set_port", { app, service, port }),
  detectApp: (path: string) => invoke<Detection>("detect_app", { path }),
  openApp: (app: string) => invoke<string>("open_app", { app }),
  mcpInfo: () => invoke<McpInfo>("mcp_info"),
  importApp: (path: string) => invoke<AppConfig>("import_app", { path }),
  exportApp: (app: string) => invoke<string>("export_app", { app }),
  showMainWindow: (select?: string) =>
    invoke<void>("show_main_window", { select }),
};

/** Native macOS folder picker; returns the chosen directory or null. */
export async function pickFolder(title?: string): Promise<string | null> {
  const res = await open({ directory: true, multiple: false, title });
  return typeof res === "string" ? res : null;
}

export const LOG_EVENT = "harbor://log";
export const STATUS_EVENT = "harbor://status";

export function onLog(cb: (l: LogLine) => void): Promise<UnlistenFn> {
  return listen<LogLine>(LOG_EVENT, (e) => cb(e.payload));
}

export function onStatus(cb: (s: StatusEvent) => void): Promise<UnlistenFn> {
  return listen<StatusEvent>(STATUS_EVENT, (e) => cb(e.payload));
}

/** Fired when the tray panel asks the main window to focus + select an app. */
export function onSelect(cb: (app: string) => void): Promise<UnlistenFn> {
  return listen<string>("harbor://select", (e) => cb(e.payload));
}
