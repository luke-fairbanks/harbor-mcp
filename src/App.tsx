import { useCallback, useEffect, useState } from "react";
import { Tooltip } from "@radix-ui/themes";
import { GearIcon, PlusIcon } from "@radix-ui/react-icons";
import { motion } from "framer-motion";
import { api, onLog, onStatus } from "./api";
import type { AppListItem, AppRunSnapshot, LogLine } from "./types";
import { StatusDot, aggregateStatus } from "./components/StatusDot";
import { AppDetail } from "./components/AppDetail";
import { SettingsPanel } from "./components/SettingsPanel";
import { RegisterDialog } from "./components/RegisterDialog";
import { AnchorMark } from "./components/icons";

const LOG_CAP = 4000;

export default function App() {
  const [items, setItems] = useState<AppListItem[]>([]);
  const [live, setLive] = useState<Record<string, AppRunSnapshot>>({});
  const [logs, setLogs] = useState<Record<string, LogLine[]>>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<"app" | "settings">("app");
  const [registerOpen, setRegisterOpen] = useState(false);

  const refreshList = useCallback(async () => {
    const list = await api.listApps();
    setItems(list);
    setLive((prev) => {
      const next = { ...prev };
      for (const it of list) if (it.run) next[it.config.name] = it.run;
      return next;
    });
    setSelected((cur) =>
      cur && list.some((i) => i.config.name === cur)
        ? cur
        : list[0]?.config.name ?? null,
    );
  }, []);

  const refreshApp = useCallback(async (app: string) => {
    const snap = await api.appStatus(app);
    setLive((prev) => ({
      ...prev,
      [app]: snap ?? { app, running: false, services: [], portPlan: [] },
    }));
  }, []);

  useEffect(() => {
    refreshList();
  }, [refreshList]);

  useEffect(() => {
    let cancelled = false;
    let offLog: (() => void) | undefined;
    let offStatus: (() => void) | undefined;

    onLog((l) => {
      setLogs((prev) => {
        const arr = prev[l.app] ? prev[l.app].concat(l) : [l];
        if (arr.length > LOG_CAP) arr.splice(0, arr.length - LOG_CAP);
        return { ...prev, [l.app]: arr };
      });
    }).then((u) => (cancelled ? u() : (offLog = u)));

    onStatus((s) => refreshApp(s.app)).then((u) =>
      cancelled ? u() : (offStatus = u),
    );

    return () => {
      cancelled = true;
      offLog?.();
      offStatus?.();
    };
  }, [refreshApp]);

  const selectedItem = items.find((i) => i.config.name === selected) ?? null;

  return (
    <div className="harbor-shell">
      <div className="drag-strip" data-tauri-drag-region />

      <aside className="harbor-sidebar">
        <div className="sidebar-head">
          <span className="sidebar-brand">
            <span style={{ color: "var(--accent)", display: "inline-flex" }}>
              <AnchorMark size={17} />
            </span>
            Harbor
          </span>
          <Tooltip content="Register an app">
            <button className="icon-btn" onClick={() => setRegisterOpen(true)}>
              <PlusIcon />
            </button>
          </Tooltip>
        </div>

        <div className="sidebar-section">Apps</div>
        <div className="applist">
          {items.length === 0 && (
            <div
              className="app-meta"
              style={{ padding: "6px 10px", lineHeight: 1.5 }}
            >
              No apps yet. Click + to register a project folder.
            </div>
          )}
          {items.map((it) => {
            const name = it.config.name;
            const status = aggregateStatus(live[name]);
            const isSel = view === "app" && selected === name;
            return (
              <div
                key={name}
                className="app-item"
                onClick={() => {
                  setSelected(name);
                  setView("app");
                }}
              >
                {isSel && (
                  <motion.div
                    layoutId="app-sel"
                    className="app-sel"
                    transition={{ type: "spring", stiffness: 520, damping: 42 }}
                  />
                )}
                <StatusDot status={status} />
                <span className="app-name">{name}</span>
                {status !== "stopped" && (
                  <span className="app-meta">{status}</span>
                )}
              </div>
            );
          })}
        </div>

        <div className="sidebar-foot">
          <button
            className="foot-btn"
            data-active={view === "settings"}
            onClick={() => setView("settings")}
          >
            <GearIcon /> Connect your Claude
          </button>
        </div>
      </aside>

      <main className="harbor-detail">
        {view === "settings" ? (
          <SettingsPanel />
        ) : selectedItem ? (
          <AppDetail
            key={selectedItem.config.name}
            item={selectedItem}
            run={live[selectedItem.config.name]}
            logs={logs[selectedItem.config.name] ?? []}
            onChanged={() => {
              refreshApp(selectedItem.config.name);
              refreshList();
            }}
            onRemoved={() => {
              setSelected(null);
              refreshList();
            }}
          />
        ) : (
          <div className="empty-state">
            <AnchorMark size={26} />
            <div>Select an app, or click + to register one.</div>
          </div>
        )}
      </main>

      <RegisterDialog
        open={registerOpen}
        onOpenChange={setRegisterOpen}
        onRegistered={(name) => {
          setSelected(name);
          setView("app");
          refreshList();
        }}
      />
    </div>
  );
}
