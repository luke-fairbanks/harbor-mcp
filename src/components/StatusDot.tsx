import type { AppRunSnapshot, ServiceStatus } from "../types";

export const STATUS_COLOR: Record<ServiceStatus, string> = {
  stopped: "var(--gray-8)",
  starting: "var(--amber-9)",
  ready: "var(--grass-9)",
  unhealthy: "var(--tomato-9)",
  exited: "var(--gray-7)",
};

export const STATUS_BADGE: Record<
  ServiceStatus,
  "gray" | "amber" | "green" | "red"
> = {
  stopped: "gray",
  starting: "amber",
  ready: "green",
  unhealthy: "red",
  exited: "gray",
};

export function StatusDot({ status }: { status: ServiceStatus }) {
  return (
    <span
      className="status-dot"
      style={{ background: STATUS_COLOR[status] }}
      data-blink={status === "starting"}
    />
  );
}

/** Roll up per-service statuses into one app-level status. */
export function aggregateStatus(run?: AppRunSnapshot): ServiceStatus {
  if (!run) return "stopped";
  const liveSvcs = run.services.filter(
    (s) => s.status !== "exited" && s.status !== "stopped",
  );
  if (!run.running || liveSvcs.length === 0) return "stopped";
  if (liveSvcs.some((s) => s.status === "starting")) return "starting";
  if (liveSvcs.some((s) => s.status === "unhealthy")) return "unhealthy";
  return "ready";
}
