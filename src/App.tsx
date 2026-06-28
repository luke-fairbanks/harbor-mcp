import { useCallback, useEffect, useState } from "react";
import { Tooltip } from "@radix-ui/themes";
import { CheckCircledIcon, GearIcon, PlusIcon } from "@radix-ui/react-icons";
import { motion } from "framer-motion";
import { api, onLog, onSelect, onStatus } from "./api";
import type { AgentStatus, AppListItem, AppRunSnapshot, LogLine } from "./types";
import { StatusDot, aggregateStatus } from "./components/StatusDot";
import { AppDetail } from "./components/AppDetail";
import { SettingsPanel } from "./components/SettingsPanel";
import { RegisterDialog } from "./components/RegisterDialog";
import { AnchorMark } from "./components/icons";
import { startWindowDrag } from "./titlebar";

const LOG_CAP = 4000;

export default function App() {
  const [items, setItems] = useState<AppListItem[]>([]);
  const [live, setLive] = useState<Record<string, AppRunSnapshot>>({});
  const [logs, setLogs] = useState<Record<string, LogLine[]>>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<"app" | "settings">("app");
  const [registerOpen, setRegisterOpen] = useState(false);
  const [agents, setAgents] = useState<AgentStatus | null>(null);

  const refreshAgents = useCallback(async () => {
    try {
      setAgents(await api.agentsStatus());
    } catch {
      /* ignore */
    }
  }, []);

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
    refreshAgents();
  }, [refreshList, refreshAgents]);

  useEffect(() => {
    let cancelled = false;
    let offLog: (() => void) | undefined;
    let offStatus: (() => void) | undefined;
    let offSelect: (() => void) | undefined;

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

    // From the tray panel: focus this window and select the app.
    onSelect((name) => {
      setSelected(name);
      setView("app");
    }).then((u) => (cancelled ? u() : (offSelect = u)));

    return () => {
      cancelled = true;
      offLog?.();
      offStatus?.();
      offSelect?.();
    };
  }, [refreshApp]);

  const selectedItem = items.find((i) => i.config.name === selected) ?? null;

  const claudeOn = !!(agents?.codeConnected || agents?.desktopConnected);
  const codexOn = !!agents?.codexConnected;
  const connectedNames = [claudeOn ? "Claude" : null, codexOn ? "Codex" : null].filter(
    Boolean,
  ) as string[];
  const agentsConnected = connectedNames.length > 0;
  const footLabel = agentsConnected
    ? `Connected to ${connectedNames.join(" & ")}`
    : "Connect your Claude";

  return (
    <div className="harbor-shell">
      <div className="drag-strip" onMouseDown={startWindowDrag} />

      <aside className="harbor-sidebar">
        <div className="sidebar-head" onMouseDown={startWindowDrag}>
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
            data-connected={agentsConnected}
            onClick={() => setView("settings")}
          >
            {agentsConnected ? <CheckCircledIcon /> : <GearIcon />} {footLabel}
          </button>
        </div>
      </aside>

      <main className="harbor-detail">
        {view === "settings" ? (
          <SettingsPanel onAgentsChanged={refreshAgents} />
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
