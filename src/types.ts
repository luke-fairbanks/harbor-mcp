// Wire types — mirror the serde output of src-tauri/src/model.rs (camelCase).

export type ServiceStatus =
  "stopped" | "starting" | "ready" | "unhealthy" | "exited";

export type HealthCheck =
  | { type: "http"; path: string; expect?: string }
  | { type: "tcp" }
  | { type: "log"; pattern: string }
  | { type: "process" };

export interface ServiceConfig {
  name: string;
  cwd: string;
  command: string;
  port?: number;
  env: Record<string, string>;
  dependsOn: string[];
  healthCheck?: HealthCheck;
  readyLogPattern?: string;
}

export interface AppConfig {
  name: string;
  root: string;
  services: ServiceConfig[];
  profiles: Record<string, string[]>;
  /** Auto-restart Harbor-spawned services that crash (bounded). Default off. */
  autoRestart?: boolean;
  /** Commands may run only after a person approves this local config. */
  trusted?: boolean;
}

export interface ServiceRun {
  name: string;
  status: ServiceStatus;
  pid?: number;
  port?: number;
  resolvedCommand?: string;
  exitCode?: number;
  /** Re-adopted from a previous Harbor session: pid/port held, no live logs. */
  adopted?: boolean;
  /** Discovered running outside Harbor (started in a terminal, etc.). */
  external?: boolean;
  /** Recent group CPU% (ps pcpu). Present while live & sampled. */
  cpu?: number;
  /** Group resident memory, in bytes. */
  memBytes?: number;
}

/** Element of the harbor://stats batch event. */
export interface ServiceStat {
  app: string;
  service: string;
  cpu: number;
  memBytes: number;
}

export interface PortPlanEntry {
  service: string;
  preferred?: number;
  resolved: number;
  note?: string;
}

export interface AppRunSnapshot {
  app: string;
  profile?: string;
  running: boolean;
  services: ServiceRun[];
  portPlan: PortPlanEntry[];
}

export interface LogLine {
  app: string;
  service: string;
  stream: "stdout" | "stderr" | "system";
  line: string;
  ts: number;
  seq: number;
}

export interface StatusEvent {
  app: string;
  service: string;
  status: ServiceStatus;
  port?: number;
  pid?: number;
  exitCode?: number;
}

export interface AppListItem {
  config: AppConfig;
  running: boolean;
  run?: AppRunSnapshot;
}

export interface McpInfo {
  url: string;
  port: number;
  token: string;
  healthy: boolean;
  version: string;
  claudeAddCommand: string;
  desktopJson: string;
  bridgeCommand: string;
}

export interface Detection {
  proposed: AppConfig;
  notes: string[];
}

export interface AgentStatus {
  codeCli: boolean;
  code: AgentConnection;
  desktopInstalled: boolean;
  desktop: AgentConnection;
  codexInstalled: boolean;
  codex: AgentConnection;
}

export interface AgentConnection {
  configured: boolean;
  bridgeRunning: boolean;
  restartRequired: boolean;
  error: string | null;
}

export interface FixResult {
  agent: string;
  response: string;
}

export interface LocalServer {
  pid: number;
  leaderPid: number;
  port: number;
  addresses: string[];
  networkExposed: boolean;
  process: string;
  command: string;
  cwd?: string;
  projectRoot?: string;
  displayName: string;
  kind: string;
  startedAt: string;
  url: string;
  httpStatus?: number;
  pageTitle?: string;
  serverHeader?: string;
  matchedApp?: string;
  matchedService?: string;
  matchReason?: string;
  tracked: boolean;
  external: boolean;
  safeToStop: boolean;
  likelyDev: boolean;
  duplicateCount: number;
  harborInternal: boolean;
}

export interface LocalServerInventory {
  scannedAt: number;
  servers: LocalServer[];
  devCount: number;
  otherCount: number;
  mappedCount: number;
  duplicateCount: number;
}
