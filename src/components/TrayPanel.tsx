import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ExternalLinkIcon,
  OpenInNewWindowIcon,
  PlayIcon,
  StopIcon,
} from "@radix-ui/react-icons";
import { api, onStatus } from "../api";
import type { AppListItem, AppRunSnapshot } from "../types";
import { StatusDot, aggregateStatus } from "./StatusDot";
import { AnchorMark } from "./icons";

export function TrayPanel() {
  const [items, setItems] = useState<AppListItem[]>([]);
  const [live, setLive] = useState<Record<string, AppRunSnapshot>>({});
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const list = await api.listApps();
    setItems(list);
    setLive((prev) => {
      const next = { ...prev };
      for (const it of list) if (it.run) next[it.config.name] = it.run;
      return next;
    });
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    onStatus(async (s) => {
      const snap = await api.appStatus(s.app);
      setLive((prev) => ({
        ...prev,
        [s.app]: snap ?? { app: s.app, running: false, services: [], portPlan: [] },
      }));
    }).then((u) => (cancelled ? u() : (off = u)));
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  const rows = useMemo(
    () =>
      items
        .map((it) => {
          const run = live[it.config.name];
          return { it, run, status: aggregateStatus(run) };
        })
        .sort(
          (a, b) =>
            (b.status !== "stopped" ? 1 : 0) - (a.status !== "stopped" ? 1 : 0) ||
            a.it.config.name.localeCompare(b.it.config.name),
        ),
    [items, live],
  );

  const runningCount = rows.filter((r) => r.status !== "stopped").length;

  async function act(name: string, fn: () => Promise<unknown>) {
    setBusy(name);
    try {
      await fn();
    } catch {
      /* surfaced in the main window logs */
    } finally {
      setBusy(null);
      refresh();
    }
  }

  return (
    <div className="tray-panel">
      <div className="tray-head">
        <span className="tray-brand">
          <span style={{ color: "var(--accent)", display: "inline-flex" }}>
            <AnchorMark size={15} />
          </span>
          Harbor
        </span>
        <span className="tray-sub">
          {runningCount > 0 ? `${runningCount} running` : "idle"}
        </span>
        <button
          className="tray-act"
          title="Open Harbor"
          onClick={() => api.showMainWindow()}
        >
          <OpenInNewWindowIcon />
        </button>
      </div>

      <div className="tray-list">
        {rows.length === 0 && (
          <div className="tray-empty">
            No apps yet. Open Harbor to register one.
          </div>
        )}
        {rows.map(({ it, run, status }) => {
          const name = it.config.name;
          const port = run?.services.find((s) => s.port != null)?.port;
          const running = status !== "stopped";
          return (
            <div className="tray-row" key={name} data-running={running}>
              <StatusDot status={status} />
              <button
                className="tray-name"
                title="Show in Harbor"
                onClick={() => api.showMainWindow(name)}
              >
                {name}
              </button>
              {running && port != null && <span className="tray-port">:{port}</span>}
              <span className="spacer" />
              {running && (
                <button
                  className="tray-act"
                  title="Open in browser"
                  onClick={() => api.openApp(name)}
                >
                  <ExternalLinkIcon />
                </button>
              )}
              {running ? (
                <button
                  className="tray-act danger"
                  disabled={busy === name}
                  title="Stop"
                  onClick={() => act(name, () => api.stopApp(name))}
                >
                  <StopIcon />
                </button>
              ) : (
                <button
                  className="tray-act go"
                  disabled={busy === name}
                  title="Start"
                  onClick={() => act(name, () => api.startApp(name))}
                >
                  <PlayIcon />
                </button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
