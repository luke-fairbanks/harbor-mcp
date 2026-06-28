// Wire types — mirror the serde output of src-tauri/src/model.rs (camelCase).

export type ServiceStatus =
  | "stopped"
  | "starting"
  | "ready"
  | "unhealthy"
  | "exited";

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
}

export interface ServiceRun {
  name: string;
  status: ServiceStatus;
  pid?: number;
  port?: number;
  resolvedCommand?: string;
  exitCode?: number;
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
  claudeAddCommand: string;
  desktopJson: string;
}

export interface Detection {
  proposed: AppConfig;
  notes: string[];
}

export interface AgentStatus {
  codeCli: boolean;
  codeConnected: boolean;
  desktopInstalled: boolean;
  desktopConnected: boolean;
  codexInstalled: boolean;
  codexConnected: boolean;
}

export interface FixResult {
  agent: string;
  response: string;
}
