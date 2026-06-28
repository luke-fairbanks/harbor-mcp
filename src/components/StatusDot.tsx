import type { AppRunSnapshot, ServiceStatus } from "../types";

export const STATUS_COLOR: Record<ServiceStatus, string> = {
  stopped: "var(--text-3)",
  starting: "var(--warn)",
  ready: "var(--ok)",
  unhealthy: "var(--danger)",
  exited: "var(--text-3)",
};

export const STATUS_TONE: Record<ServiceStatus, string> = {
  stopped: "default",
  starting: "warn",
  ready: "ok",
  unhealthy: "danger",
  exited: "default",
};

export function StatusDot({ status }: { status: ServiceStatus }) {
  const color = STATUS_COLOR[status];
  return (
    <span
      className="status-dot"
      style={{ color, background: color }}
      data-pulse={status === "starting"}
    />
  );
}

export function StatusBadge({ status }: { status: ServiceStatus }) {
  const tone = STATUS_TONE[status];
  const color = STATUS_COLOR[status];
  return (
    <span
      className="badge"
      data-tone={tone}
      style={{
        background: `color-mix(in srgb, ${color} 16%, transparent)`,
        color,
      }}
    >
      {status}
    </span>
  );
}

/** Roll up per-service statuses into one app-level status. */
export function aggregateStatus(run?: AppRunSnapshot): ServiceStatus {
  if (!run) return "stopped";
  const live = run.services.filter(
    (s) => s.status !== "exited" && s.status !== "stopped",
  );
  if (!run.running || live.length === 0) return "stopped";
  if (live.some((s) => s.status === "starting")) return "starting";
  if (live.some((s) => s.status === "unhealthy")) return "unhealthy";
  return "ready";
}
