import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AgentStatus,
  AppConfig,
  AppListItem,
  AppRunSnapshot,
  Detection,
  FixResult,
  LogLine,
  LocalServerInventory,
  McpInfo,
  ServiceStat,
  StatusEvent,
} from "./types";

export const api = {
  listApps: () => invoke<AppListItem[]>("list_apps"),
  appStatus: (app: string) =>
    invoke<AppRunSnapshot | null>("app_status", { app }),
  listLocalServers: () => invoke<LocalServerInventory>("list_local_servers"),
  stopLocalServer: (pid: number, port: number, startedAt: string) =>
    invoke<void>("stop_local_server", { pid, port, startedAt }),
  startApp: (app: string, profile?: string) =>
    invoke<AppRunSnapshot>("start_app", { app, profile }),
  stopApp: (app: string) => invoke<void>("stop_app", { app }),
  restartApp: (app: string, profile?: string) =>
    invoke<AppRunSnapshot>("restart_app", { app, profile }),
  startAll: () => invoke<void>("start_all"),
  stopAll: () => invoke<void>("stop_all"),
  getLogs: (app: string, service: string, lines?: number) =>
    invoke<LogLine[]>("get_logs", { app, service, lines }),
  registerApp: (config: AppConfig) => invoke<void>("register_app", { config }),
  approveApp: (app: string, expected: AppConfig) =>
    invoke<void>("approve_app", { app, expected }),
  updateApp: (app: string, config: AppConfig) =>
    invoke<void>("update_app", { app, config }),
  removeApp: (app: string) => invoke<boolean>("remove_app", { app }),
  setEnv: (app: string, service: string, env: Record<string, string>) =>
    invoke<boolean>("set_env", { app, service, env }),
  setPort: (app: string, service: string, port: number) =>
    invoke<boolean>("set_port", { app, service, port }),
  detectApp: (path: string) => invoke<Detection>("detect_app", { path }),
  pathKind: (path: string) =>
    invoke<"dir" | "file" | "missing">("path_kind", { path }),
  readDotenv: (path: string) =>
    invoke<Record<string, string>>("read_dotenv", { path }),
  openApp: (app: string) => invoke<string>("open_app", { app }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  mcpInfo: () => invoke<McpInfo>("mcp_info"),
  importApp: (path: string) => invoke<AppConfig>("import_app", { path }),
  exportApp: (app: string) => invoke<string>("export_app", { app }),
  showMainWindow: (select?: string) =>
    invoke<void>("show_main_window", { select }),
  agentsStatus: () => invoke<AgentStatus>("agents_status"),
  connectClaudeCode: () => invoke<string>("connect_claude_code"),
  connectClaudeDesktop: () => invoke<string>("connect_claude_desktop"),
  connectCodex: () => invoke<string>("connect_codex"),
  fixPrompt: (app: string, service: string) =>
    invoke<string>("fix_prompt", { app, service }),
  runFix: (app: string, service: string) =>
    invoke<FixResult>("run_fix", { app, service }),
};

/** Native macOS folder picker; returns the chosen directory or null. */
export async function pickFolder(title?: string): Promise<string | null> {
  const res = await open({ directory: true, multiple: false, title });
  return typeof res === "string" ? res : null;
}

/** Native picker for a .env file; returns the chosen path or null. */
export async function pickEnvFile(): Promise<string | null> {
  const res = await open({
    multiple: false,
    title: "Choose a .env file",
    filters: [{ name: "env", extensions: ["env", "txt", "*"] }],
  });
  return typeof res === "string" ? res : null;
}

/** Compact memory label: "84 MB", "1.2 GB" (summed group RSS, approximate). */
export function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  const u = ["KB", "MB", "GB"];
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${u[i]}`;
}

export const LOG_EVENT = "harbor://log";
export const STATUS_EVENT = "harbor://status";
export const STATS_EVENT = "harbor://stats";

export function onStats(cb: (s: ServiceStat[]) => void): Promise<UnlistenFn> {
  return listen<ServiceStat[]>(STATS_EVENT, (e) => cb(e.payload));
}

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

/** Fired when the registry changes (e.g. an app registered over MCP). */
export function onRegistry(cb: () => void): Promise<UnlistenFn> {
  return listen("harbor://registry", () => cb());
}
