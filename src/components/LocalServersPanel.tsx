import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Code, Spinner, Switch, Tooltip } from "@radix-ui/themes";
import {
  ExternalLinkIcon,
  PlusIcon,
  ReloadIcon,
  StopIcon,
} from "@radix-ui/react-icons";
import { api } from "../api";
import type { LocalServer, LocalServerInventory } from "../types";
import { ConfirmDialog } from "./ConfirmDialog";
import { startWindowDrag } from "../titlebar";

function serverState(server: LocalServer): { label: string; tone: string } {
  if (server.harborInternal) return { label: "Harbor", tone: "accent" };
  if (server.tracked && server.external)
    return { label: "mapped · external", tone: "ok" };
  if (server.tracked) return { label: "managed", tone: "ok" };
  if (server.matchedApp)
    return { label: "matched · monitor only", tone: "accent" };
  return { label: "unmapped", tone: "warn" };
}

export function LocalServersPanel({
  onOpenApp,
  onRegisterPath,
}: {
  onOpenApp: (name: string) => void;
  onRegisterPath: (path: string) => void;
}) {
  const [inventory, setInventory] = useState<LocalServerInventory | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [confirmStop, setConfirmStop] = useState<LocalServer | null>(null);
  const [stopping, setStopping] = useState<number | null>(null);
  const refreshGeneration = useRef(0);
  const refreshInFlight = useRef(false);
  const refreshQueued = useRef(false);

  const refresh = useCallback(async (quiet = false) => {
    if (refreshInFlight.current) {
      // Drop interval ticks while busy. Explicit/manual refreshes queue exactly
      // one follow-up pass, so a slow scan cannot become a permanent loop.
      if (!quiet) refreshQueued.current = true;
      return;
    }
    refreshInFlight.current = true;
    if (!quiet) setLoading(true);
    setError(null);
    try {
      do {
        refreshQueued.current = false;
        const generation = ++refreshGeneration.current;
        try {
          const next = await api.listLocalServers();
          if (generation === refreshGeneration.current) setInventory(next);
        } catch (e) {
          if (generation === refreshGeneration.current) setError(String(e));
        }
      } while (refreshQueued.current);
    } finally {
      refreshInFlight.current = false;
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    const timer = window.setInterval(() => refresh(true), 8_000);
    return () => {
      window.clearInterval(timer);
      refreshQueued.current = false;
      refreshGeneration.current++;
    };
  }, [refresh]);

  const visible = useMemo(
    () =>
      (inventory?.servers ?? []).filter(
        (server) => showAll || server.likelyDev || server.harborInternal,
      ),
    [inventory, showAll],
  );

  async function stop(server: LocalServer) {
    setStopping(server.leaderPid);
    setError(null);
    try {
      await api.stopLocalServer(
        server.leaderPid,
        server.port,
        server.startedAt,
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setStopping(null);
      setConfirmStop(null);
    }
  }

  return (
    <>
      <div
        className="detail-head local-servers-head"
        onMouseDown={startWindowDrag}
      >
        <div>
          <div className="detail-title">Local servers</div>
          <div className="detail-sub">
            TCP listeners owned by your macOS user, matched to projects when
            Harbor has evidence.
          </div>
        </div>
        <Tooltip content="Scan again">
          <button
            className="icon-btn"
            data-no-drag
            onClick={() => refresh()}
            disabled={loading}
          >
            <ReloadIcon className={loading ? "spin" : undefined} />
          </button>
        </Tooltip>
      </div>

      <div className="detail-body local-servers-body">
        <div className="server-summary">
          <span className="chip" data-tone="ok">
            {inventory?.devCount ?? 0} dev servers
          </span>
          <span className="chip" data-tone="accent">
            {inventory?.mappedCount ?? 0} mapped
          </span>
          {(inventory?.duplicateCount ?? 0) > 0 && (
            <span className="chip" data-tone="warn">
              {inventory!.duplicateCount} probable duplicate
              {inventory!.duplicateCount === 1 ? "" : "s"}
            </span>
          )}
          <label className="server-show-all">
            <Switch size="1" checked={showAll} onCheckedChange={setShowAll} />
            Show {inventory?.otherCount ?? 0} other listeners
          </label>
        </div>

        {error && <div className="server-error mono">{error}</div>}

        {loading && !inventory ? (
          <div className="server-loading">
            <Spinner /> Inspecting local listeners…
          </div>
        ) : visible.length === 0 ? (
          <div className="server-empty">
            No local development servers are listening right now.
          </div>
        ) : (
          <div className="server-list">
            {visible.map((server) => {
              const state = serverState(server);
              const canOpen = server.httpStatus != null;
              return (
                <div
                  className="server-card"
                  data-duplicate={server.duplicateCount > 1 || undefined}
                  key={`${server.pid}:${server.port}`}
                >
                  <div className="server-card-main">
                    <div className="server-card-title-row">
                      <div className="server-card-title">
                        {server.displayName}
                        {canOpen ? (
                          <button
                            className="server-port"
                            onClick={() => api.openUrl(server.url)}
                            title={`Open ${server.url}`}
                          >
                            :{server.port}
                          </button>
                        ) : (
                          <span className="server-port">:{server.port}</span>
                        )}
                      </div>
                      <div className="row" style={{ gap: 6, flex: "none" }}>
                        {server.duplicateCount > 1 && (
                          <span className="chip" data-tone="warn">
                            {server.duplicateCount} similar runs
                          </span>
                        )}
                        {server.networkExposed && (
                          <Tooltip content="This socket is not loopback-only and may be reachable by other devices, depending on your firewall.">
                            <span className="chip" data-tone="warn">
                              network-visible
                            </span>
                          </Tooltip>
                        )}
                        <span className="chip" data-tone={state.tone}>
                          {state.label}
                        </span>
                      </div>
                    </div>

                    <div className="server-description">
                      {server.pageTitle || server.kind}
                      {server.pageTitle && <span> · {server.kind}</span>}
                      {server.httpStatus && (
                        <span> · HTTP {server.httpStatus}</span>
                      )}
                    </div>

                    <div className="server-meta">
                      <span>PID {server.pid}</span>
                      {server.leaderPid !== server.pid && (
                        <span>process group {server.leaderPid}</span>
                      )}
                      <span>{server.process}</span>
                      <span title={server.addresses.join(", ")}>
                        {server.networkExposed
                          ? "non-loopback bind"
                          : "loopback-only"}
                      </span>
                      {server.matchedService && (
                        <span>service {server.matchedService}</span>
                      )}
                      {server.matchReason && <span>{server.matchReason}</span>}
                      <span title="Process start time">
                        started {server.startedAt}
                      </span>
                    </div>
                    <Code className="server-command" title={server.command}>
                      {server.command}
                    </Code>
                    {(server.cwd || server.projectRoot) && (
                      <div className="server-path mono" title={server.cwd}>
                        {server.projectRoot || server.cwd}
                      </div>
                    )}
                  </div>

                  <div className="server-actions">
                    {canOpen && (
                      <Button
                        size="1"
                        variant="soft"
                        onClick={() => api.openUrl(server.url)}
                      >
                        <ExternalLinkIcon /> Open
                      </Button>
                    )}
                    {server.matchedApp && !server.harborInternal && (
                      <Button
                        size="1"
                        variant="soft"
                        color="gray"
                        onClick={() => onOpenApp(server.matchedApp!)}
                      >
                        View app
                      </Button>
                    )}
                    {!server.matchedApp && server.projectRoot && (
                      <Button
                        size="1"
                        variant="soft"
                        color="gray"
                        onClick={() => onRegisterPath(server.projectRoot!)}
                      >
                        <PlusIcon /> Add to Harbor
                      </Button>
                    )}
                    {server.safeToStop && (
                      <Button
                        size="1"
                        variant="soft"
                        color="red"
                        disabled={stopping === server.leaderPid}
                        onClick={() => setConfirmStop(server)}
                      >
                        {stopping === server.leaderPid ? (
                          <Spinner size="1" />
                        ) : (
                          <StopIcon />
                        )}
                        Stop
                      </Button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      <ConfirmDialog
        open={!!confirmStop}
        onOpenChange={(open) => !open && setConfirmStop(null)}
        title={`Stop ${confirmStop?.displayName ?? "local server"}?`}
        body={
          <>
            Harbor will stop the isolated process group listening on port{" "}
            <Code>{confirmStop?.port}</Code>. It will re-check the PID and start
            time first, and refuses to stop shells, terminals, IDEs, or coding
            agents.
          </>
        }
        confirmLabel="Stop server"
        danger
        onConfirm={() => confirmStop && stop(confirmStop)}
      />
    </>
  );
}
