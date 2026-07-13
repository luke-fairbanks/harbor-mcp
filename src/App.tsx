import { useCallback, useEffect, useRef, useState } from "react";
import { DropdownMenu, Tooltip } from "@radix-ui/themes";
import {
  CheckCircledIcon,
  DashboardIcon,
  DotsHorizontalIcon,
  GlobeIcon,
  Link2Icon,
  PlusIcon,
} from "@radix-ui/react-icons";
import { AnimatePresence, motion } from "framer-motion";
import { api, onLog, onRegistry, onSelect, onStats, onStatus } from "./api";
import type {
  AgentStatus,
  AppListItem,
  AppRunSnapshot,
  Detection,
  LogLine,
} from "./types";
import { StatusDot, aggregateStatus } from "./components/StatusDot";
import { AppDetail } from "./components/AppDetail";
import { SettingsPanel } from "./components/SettingsPanel";
import { LocalServersPanel } from "./components/LocalServersPanel";
import { OverviewPanel } from "./components/OverviewPanel";
import { RegisterDialog } from "./components/RegisterDialog";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { AnchorMark, ProjectGlyph } from "./components/icons";
import { useFolderDrop } from "./useDragDrop";
import { startWindowDrag } from "./titlebar";

const LOG_CAP = 4000;

export default function App() {
  const [items, setItems] = useState<AppListItem[]>([]);
  const [live, setLive] = useState<Record<string, AppRunSnapshot>>({});
  const [logs, setLogs] = useState<Record<string, LogLine[]>>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<"overview" | "app" | "servers" | "settings">(
    "overview",
  );
  const [registerOpen, setRegisterOpen] = useState(false);
  const [agents, setAgents] = useState<AgentStatus | null>(null);
  const [confirmStopAll, setConfirmStopAll] = useState(false);
  const [pendingDetection, setPendingDetection] = useState<Detection | null>(
    null,
  );
  const [scanning, setScanning] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  const listRequestGeneration = useRef(0);
  const requestSequence = useRef(0);
  const lastAppliedSequence = useRef<Record<string, number>>({});

  const dragging = useFolderDrop(async (path) => {
    const kind = await api.pathKind(path);
    if (kind === "missing") return;
    // Dropped a file (e.g. package.json/.env) → register its parent folder.
    const dir = kind === "dir" ? path : path.replace(/\/[^/]+$/, "");
    setPendingDetection(null);
    setDropError(null);
    setScanning(true);
    setRegisterOpen(true);
    try {
      setPendingDetection(await api.detectApp(dir));
    } catch (e) {
      setDropError(String(e));
    } finally {
      setScanning(false);
    }
  });

  const refreshAgents = useCallback(async () => {
    try {
      setAgents(await api.agentsStatus());
    } catch {
      /* ignore */
    }
  }, []);

  const refreshList = useCallback(async () => {
    const generation = ++listRequestGeneration.current;
    const request = ++requestSequence.current;
    const list = await api.listApps();
    if (generation !== listRequestGeneration.current) return;
    setItems(list);
    setLive((previous) => {
      const next: Record<string, AppRunSnapshot> = {};
      for (const it of list) {
        const name = it.config.name;
        if (
          request < (lastAppliedSequence.current[name] ?? 0) &&
          previous[name]
        ) {
          next[name] = previous[name];
          continue;
        }
        lastAppliedSequence.current[name] = request;
        next[name] = it.run ?? {
          app: name,
          running: false,
          services: [],
          portPlan: [],
        };
      }
      return next;
    });
    setSelected((cur) =>
      cur && list.some((i) => i.config.name === cur)
        ? cur
        : (list[0]?.config.name ?? null),
    );
  }, []);

  const refreshApp = useCallback(async (app: string, hydrate = false) => {
    const request = ++requestSequence.current;
    const snap = await api.appStatus(app);
    if (request >= (lastAppliedSequence.current[app] ?? 0)) {
      lastAppliedSequence.current[app] = request;
      setLive((prev) => ({
        ...prev,
        [app]: snap ?? { app, running: false, services: [], portPlan: [] },
      }));
    }
    if (hydrate && snap) {
      const history = (
        await Promise.all(
          snap.services.map((service) => api.getLogs(app, service.name, 300)),
        )
      ).flat();
      if (history.length > 0) {
        setLogs((prev) => {
          const merged = [...(prev[app] ?? []), ...history];
          const unique = new Map(merged.map((line) => [line.seq, line]));
          const ordered = [...unique.values()].sort((a, b) => a.seq - b.seq);
          return { ...prev, [app]: ordered.slice(-LOG_CAP) };
        });
      }
    }
  }, []);

  useEffect(() => {
    refreshList();
    refreshAgents();
  }, [refreshList, refreshAgents]);

  // Hydrate early/adopted logs when an app is selected; live events alone miss
  // output emitted before the detail view subscribed.
  useEffect(() => {
    if (view === "app" && selected) {
      refreshApp(selected, true);
    }
  }, [view, selected, refreshApp]);

  useEffect(() => {
    let cancelled = false;
    let offLog: (() => void) | undefined;
    let offStatus: (() => void) | undefined;
    let offSelect: (() => void) | undefined;
    let offRegistry: (() => void) | undefined;
    let offStats: (() => void) | undefined;

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

    // An app was registered/updated (e.g. over MCP) — refresh the list.
    onRegistry(() => refreshList()).then((u) =>
      cancelled ? u() : (offRegistry = u),
    );

    // Resource samples — patch cpu/mem into `live` in place, no IPC round-trip.
    onStats((stats) => {
      setLive((prev) => {
        const next = { ...prev };
        for (const st of stats) {
          const snap = next[st.app];
          if (!snap) continue;
          next[st.app] = {
            ...snap,
            services: snap.services.map((s) =>
              s.name === st.service
                ? { ...s, cpu: st.cpu, memBytes: st.memBytes }
                : s,
            ),
          };
        }
        return next;
      });
    }).then((u) => (cancelled ? u() : (offStats = u)));

    return () => {
      cancelled = true;
      offLog?.();
      offStatus?.();
      offSelect?.();
      offRegistry?.();
      offStats?.();
    };
  }, [refreshApp, refreshList]);

  const selectedItem = items.find((i) => i.config.name === selected) ?? null;

  const claudeOn = !!(agents?.codeConnected || agents?.desktopConnected);
  const codexOn = !!agents?.codexConnected;
  const connectedNames = [
    claudeOn ? "Claude" : null,
    codexOn ? "Codex" : null,
  ].filter(Boolean) as string[];
  const agentsConnected = connectedNames.length > 0;
  const bridgeLabel = agentsConnected
    ? `${connectedNames.length} connected`
    : "Not connected";

  const runningCount = items.filter(
    (item) => live[item.config.name]?.running ?? item.running,
  ).length;
  const startableCount = items.filter(
    (item) =>
      item.config.trusted !== false &&
      !(live[item.config.name]?.running ?? item.running),
  ).length;

  async function doStartAll() {
    try {
      await api.startAll();
    } finally {
      refreshList();
    }
  }
  async function doStopAll() {
    try {
      await api.stopAll();
    } finally {
      refreshList();
    }
  }

  async function registerDetectedPath(path: string) {
    setPendingDetection(null);
    setDropError(null);
    setScanning(true);
    setRegisterOpen(true);
    try {
      setPendingDetection(await api.detectApp(path));
    } catch (e) {
      setDropError(String(e));
    } finally {
      setScanning(false);
    }
  }

  return (
    <div className="harbor-shell" data-dragging={dragging || undefined}>
      <div className="drag-strip" onMouseDown={startWindowDrag} />
      {dragging && (
        <div className="drop-overlay">
          <AnchorMark size={30} />
          <div>Drop a project folder to register it</div>
        </div>
      )}

      <aside className="harbor-sidebar">
        <div className="sidebar-head" onMouseDown={startWindowDrag}>
          <span className="sidebar-brand">
            <span className="sidebar-brand-mark">
              <AnchorMark size={17} />
            </span>
            <span className="sidebar-brand-copy">
              <strong>Harbor</strong>
              <small>Local control deck</small>
            </span>
          </span>
          <DropdownMenu.Root>
            <DropdownMenu.Trigger>
              <button
                className="sidebar-icon-btn"
                aria-label="Harbor actions"
                data-no-drag
              >
                <DotsHorizontalIcon />
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Content size="1">
              <DropdownMenu.Item
                disabled={startableCount === 0}
                onSelect={doStartAll}
              >
                Start approved projects
              </DropdownMenu.Item>
              <DropdownMenu.Item
                color="red"
                disabled={runningCount === 0}
                onSelect={() => setConfirmStopAll(true)}
              >
                Stop all running projects
              </DropdownMenu.Item>
            </DropdownMenu.Content>
          </DropdownMenu.Root>
        </div>

        <div
          className="sidebar-running"
          aria-label={`${runningCount} projects running`}
        >
          <span
            className="sidebar-running-pulse"
            data-active={runningCount > 0 || undefined}
          />
          <span>
            {runningCount > 0 ? `${runningCount} running` : "All quiet"}
          </span>
          <span className="sidebar-running-total">{items.length} projects</span>
        </div>

        <nav className="sidebar-nav" aria-label="Harbor navigation">
          <button
            className="sidebar-nav-item"
            data-active={view === "overview"}
            aria-current={view === "overview" ? "page" : undefined}
            onClick={() => setView("overview")}
          >
            <DashboardIcon />
            <span>Overview</span>
          </button>
          <button
            className="sidebar-nav-item"
            data-active={view === "servers"}
            aria-current={view === "servers" ? "page" : undefined}
            onClick={() => setView("servers")}
          >
            <GlobeIcon />
            <span>Local servers</span>
          </button>
          <button
            className="sidebar-nav-item"
            data-active={view === "settings"}
            aria-current={view === "settings" ? "page" : undefined}
            onClick={() => setView("settings")}
          >
            <Link2Icon />
            <span>AI connections</span>
            {agentsConnected && (
              <span className="sidebar-nav-badge">{connectedNames.length}</span>
            )}
          </button>
        </nav>

        <div className="sidebar-section-heading">
          <span>Projects</span>
          <Tooltip content="Add a project">
            <button
              className="sidebar-add-btn"
              aria-label="Add a project"
              onClick={() => setRegisterOpen(true)}
            >
              <PlusIcon />
            </button>
          </Tooltip>
        </div>

        <nav className="applist" aria-label="Projects">
          {items.length === 0 && (
            <button
              className="sidebar-empty-projects"
              onClick={() => setRegisterOpen(true)}
            >
              <PlusIcon />
              <span>
                <strong>Add your first project</strong>
                <small>Harbor will detect its services.</small>
              </span>
            </button>
          )}
          {items.map((it) => {
            const name = it.config.name;
            const status = aggregateStatus(live[name]);
            const isSel = view === "app" && selected === name;
            const statusLabel =
              it.config.trusted === false
                ? "Review required"
                : status === "stopped"
                  ? "Idle"
                  : status;
            return (
              <button
                key={name}
                className="app-item"
                data-active={isSel || undefined}
                aria-current={isSel ? "page" : undefined}
                aria-label={`${name}, ${statusLabel}`}
                onClick={() => {
                  setSelected(name);
                  setView("app");
                }}
              >
                {isSel && (
                  <motion.span
                    layoutId="app-sel"
                    className="app-sel"
                    transition={{ type: "spring", stiffness: 420, damping: 38 }}
                  />
                )}
                <ProjectGlyph name={name} compact />
                <span className="app-item-copy">
                  <span className="app-name">{name}</span>
                  <span
                    className="app-meta"
                    data-attention={it.config.trusted === false || undefined}
                  >
                    {statusLabel}
                  </span>
                </span>
                <StatusDot status={status} />
              </button>
            );
          })}
        </nav>

        <div className="sidebar-foot">
          <button
            className="sidebar-bridge-status"
            data-connected={agentsConnected || undefined}
            onClick={() => setView("settings")}
            aria-label={`AI connections: ${bridgeLabel}`}
          >
            <span className="bridge-status-icon">
              {agentsConnected ? <CheckCircledIcon /> : <Link2Icon />}
            </span>
            <span>
              <strong>Harbor Bridge</strong>
              <small>{bridgeLabel}</small>
            </span>
          </button>
        </div>
      </aside>

      <main className="harbor-detail">
        <AnimatePresence mode="wait" initial={false}>
          <motion.section
            className="view-frame"
            key={view === "app" ? `app:${selected ?? "none"}` : view}
            initial={{ opacity: 0, y: 4 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -2 }}
            transition={{ duration: 0.14, ease: [0.22, 1, 0.36, 1] }}
          >
            {view === "overview" ? (
              <OverviewPanel
                items={items}
                live={live}
                agents={agents}
                onOpenApp={(name) => {
                  setSelected(name);
                  setView("app");
                }}
                onAddProject={() => setRegisterOpen(true)}
                onOpenServers={() => setView("servers")}
                onOpenConnections={() => setView("settings")}
              />
            ) : view === "settings" ? (
              <SettingsPanel onAgentsChanged={refreshAgents} />
            ) : view === "servers" ? (
              <LocalServersPanel
                onOpenApp={(name) => {
                  setSelected(name);
                  setView("app");
                }}
                onRegisterPath={registerDetectedPath}
              />
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
                  setView("overview");
                  refreshList();
                }}
              />
            ) : (
              <OverviewPanel
                items={items}
                live={live}
                agents={agents}
                onOpenApp={(name) => {
                  setSelected(name);
                  setView("app");
                }}
                onAddProject={() => setRegisterOpen(true)}
                onOpenServers={() => setView("servers")}
                onOpenConnections={() => setView("settings")}
              />
            )}
          </motion.section>
        </AnimatePresence>
      </main>

      <RegisterDialog
        open={registerOpen}
        onOpenChange={(v) => {
          setRegisterOpen(v);
          if (!v) {
            setPendingDetection(null);
            setDropError(null);
            setScanning(false);
          }
        }}
        initialDetection={pendingDetection}
        initialError={dropError}
        scanning={scanning}
        onRegistered={(name) => {
          setSelected(name);
          setView("app");
          refreshList();
        }}
      />

      <ConfirmDialog
        open={confirmStopAll}
        onOpenChange={setConfirmStopAll}
        title="Stop all running apps?"
        body="Sends SIGTERM then SIGKILL to every running app's process group, including any servers started outside Harbor."
        confirmLabel="Stop all"
        danger
        onConfirm={doStopAll}
      />
    </div>
  );
}
