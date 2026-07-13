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

export const STATUS_LABEL: Record<ServiceStatus, string> = {
  stopped: "Stopped",
  starting: "Starting",
  ready: "Ready",
  unhealthy: "Needs attention",
  exited: "Exited",
};

export function StatusDot({ status }: { status: ServiceStatus }) {
  const color = STATUS_COLOR[status];
  return (
    <span
      className="status-dot"
      style={{ color, background: color }}
      data-pulse={status === "starting"}
      aria-hidden="true"
    />
  );
}

export function StatusBadge({ status }: { status: ServiceStatus }) {
  const tone = STATUS_TONE[status];
  const color = STATUS_COLOR[status];
  const label = STATUS_LABEL[status];
  return (
    <span
      className="badge"
      data-tone={tone}
      aria-label={`Service status: ${label}`}
      style={{
        background: `color-mix(in srgb, ${color} 16%, transparent)`,
        color,
      }}
    >
      {label}
    </span>
  );
}

/** Roll up per-service statuses into one app-level status. */
export function aggregateStatus(run?: AppRunSnapshot): ServiceStatus {
  if (!run) return "stopped";
  const live = run.services.filter(
    (s) => s.status !== "exited" && s.status !== "stopped",
  );
  if (!run.running || live.length === 0) {
    // A non-running app that has a service which crashed (non-zero exit) reads
    // as a red "unhealthy" dot, not a neutral grey "stopped".
    const crashed = run.services.some(
      (s) => s.status === "exited" && s.exitCode != null && s.exitCode !== 0,
    );
    return crashed ? "unhealthy" : "stopped";
  }
  if (live.some((s) => s.status === "starting")) return "starting";
  if (live.some((s) => s.status === "unhealthy")) return "unhealthy";
  return "ready";
}
