import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Code, Spinner, Switch, Tooltip } from "@radix-ui/themes";
import {
  ExternalLinkIcon,
  FileIcon,
  GlobeIcon,
  PlusIcon,
  ReloadIcon,
  StopIcon,
} from "@radix-ui/react-icons";
import { api } from "../api";
import type { LocalServer, LocalServerInventory } from "../types";
import { ConfirmDialog } from "./ConfirmDialog";
import { startWindowDrag } from "../titlebar";
import { HarborBeacon } from "./icons";

function serverState(server: LocalServer): {
  label: string;
  tone: string;
  description: string;
} {
  if (server.harborInternal)
    return {
      label: "Harbor",
      tone: "accent",
      description: "Harbor's private MCP listener.",
    };
  if (server.tracked && server.external)
    return {
      label: "Mapped",
      tone: "ok",
      description: "Started outside Harbor and safely mapped to this project.",
    };
  if (server.tracked)
    return {
      label: "Managed",
      tone: "ok",
      description: "Started and managed by Harbor.",
    };
  if (server.matchedApp)
    return {
      label: "Project found",
      tone: "accent",
      description:
        "Matches a Harbor project, but Harbor is only monitoring it.",
    };
  return {
    label: "Not in Harbor",
    tone: "neutral",
    description: "This listener has not been connected to a Harbor project.",
  };
}

function isRegisterableProjectRoot(path: string): boolean {
  const normalized = path.replace(/\/+$/, "") || "/";
  return (
    normalized !== "/" &&
    normalized !== "/opt/homebrew" &&
    normalized !== "/usr/local" &&
    !/^\/Users\/[^/]+$/.test(normalized) &&
    !normalized.startsWith("/Applications/")
  );
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
    if (!quiet) setError(null);
    try {
      do {
        refreshQueued.current = false;
        const generation = ++refreshGeneration.current;
        try {
          const next = await api.listLocalServers();
          if (generation === refreshGeneration.current) setInventory(next);
        } catch (e) {
          if (!quiet && generation === refreshGeneration.current)
            setError(String(e));
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

  const visible = useMemo(() => {
    const servers = (inventory?.servers ?? []).filter(
      (server) => showAll || server.likelyDev || server.harborInternal,
    );
    const attentionScore = (server: LocalServer) =>
      (server.duplicateCount > 1 ? 4 : 0) +
      (server.networkExposed ? 2 : 0) +
      (!server.tracked && !server.harborInternal ? 1 : 0);
    return servers.sort((a, b) => attentionScore(b) - attentionScore(a));
  }, [inventory, showAll]);

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

  async function openServer(server: LocalServer) {
    setError(null);
    try {
      await api.openUrl(server.url);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <>
      <div
        className="detail-head local-servers-head"
        onMouseDown={startWindowDrag}
      >
        <div className="page-title-block">
          <div className="page-eyebrow">Machine inventory</div>
          <h1 className="detail-title">Local servers</h1>
          <div className="detail-sub">
            See every development listener, spot duplicates, and bring the right
            process under Harbor control.
          </div>
        </div>
        <Tooltip content="Scan again">
          <button
            className="icon-btn"
            onClick={() => refresh()}
            disabled={loading}
            aria-label="Scan local servers again"
          >
            <ReloadIcon className={loading ? "spin" : undefined} />
          </button>
        </Tooltip>
      </div>

      <div className="detail-body local-servers-body" aria-busy={loading}>
        <div className="server-summary">
          <div className="server-metrics" aria-label="Local server summary">
            <div className="server-metric" data-tone="accent">
              <strong>{inventory?.devCount ?? 0}</strong>
              <span>Development</span>
            </div>
            <div className="server-metric" data-tone="ok">
              <strong>{inventory?.mappedCount ?? 0}</strong>
              <span>Mapped</span>
            </div>
            <div
              className="server-metric"
              data-tone={
                (inventory?.duplicateCount ?? 0) > 0 ? "warn" : "quiet"
              }
            >
              <strong>{inventory?.duplicateCount ?? 0}</strong>
              <span>Duplicates</span>
            </div>
          </div>
          <label className="server-show-all">
            <Switch
              size="1"
              checked={showAll}
              onCheckedChange={setShowAll}
              disabled={(inventory?.otherCount ?? 0) === 0}
            />
            Include {inventory?.otherCount ?? 0} system listeners
          </label>
        </div>

        {error && (
          <div className="server-error mono" role="alert">
            {error}
          </div>
        )}

        {loading && !inventory ? (
          <div className="server-loading" role="status">
            <Spinner /> Inspecting local listeners…
          </div>
        ) : visible.length === 0 ? (
          <div className="server-empty">
            <HarborBeacon size={82} />
            <strong>The coast is clear</strong>
            <span>No local development servers are listening right now.</span>
            <Button size="2" variant="soft" onClick={() => refresh()}>
              <ReloadIcon /> Scan again
            </Button>
          </div>
        ) : (
          <div className="server-list" role="list">
            {visible.map((server) => {
              const state = serverState(server);
              const canOpen = server.httpStatus != null;
              const canViewApp = !!server.matchedApp && !server.harborInternal;
              const canAdd =
                !server.matchedApp &&
                !!server.projectRoot &&
                isRegisterableProjectRoot(server.projectRoot) &&
                (canOpen || server.safeToStop);
              const hasActions =
                canOpen || canViewApp || canAdd || server.safeToStop;
              const displayedPath = server.projectRoot || server.cwd;
              return (
                <article
                  className="server-card"
                  data-duplicate={server.duplicateCount > 1 || undefined}
                  data-state={state.tone}
                  data-exposed={server.networkExposed || undefined}
                  key={`${server.pid}:${server.port}`}
                  role="listitem"
                >
                  <div className="server-card-main">
                    <div className="server-card-title-row">
                      <div className="server-identity">
                        <span className="server-process-mark" aria-hidden>
                          <GlobeIcon />
                        </span>
                        <div className="server-identity-copy">
                          <h2 className="server-card-title">
                            {server.displayName}
                            {canOpen ? (
                              <button
                                className="server-port"
                                onClick={() => openServer(server)}
                                title={`Open ${server.url}`}
                                aria-label={`Open ${server.displayName} on port ${server.port}`}
                              >
                                :{server.port}
                                <ExternalLinkIcon />
                              </button>
                            ) : (
                              <span className="server-port">
                                :{server.port}
                              </span>
                            )}
                          </h2>
                          <div className="server-description">
                            {server.pageTitle || server.kind}
                            {server.pageTitle && <span> · {server.kind}</span>}
                            {server.httpStatus && (
                              <span> · HTTP {server.httpStatus}</span>
                            )}
                          </div>
                        </div>
                      </div>
                      <div className="server-badges">
                        {server.duplicateCount > 1 && (
                          <span className="chip" data-tone="warn">
                            {server.duplicateCount} similar runs
                          </span>
                        )}
                        {server.networkExposed && (
                          <Tooltip content="This socket is not loopback-only and may be reachable by other devices, depending on your firewall.">
                            <span
                              className="chip"
                              data-tone="warn"
                              tabIndex={0}
                              aria-label="Network visible: this socket may be reachable by other devices, depending on your firewall"
                            >
                              Network visible
                            </span>
                          </Tooltip>
                        )}
                        <Tooltip content={state.description}>
                          <span
                            className="chip"
                            data-tone={state.tone}
                            tabIndex={0}
                          >
                            {state.label}
                          </span>
                        </Tooltip>
                      </div>
                    </div>

                    {displayedPath && (
                      <div className="server-path mono" title={displayedPath}>
                        <FileIcon aria-hidden />
                        {displayedPath}
                      </div>
                    )}

                    <details className="server-details">
                      <summary>Technical details</summary>
                      <div className="server-details-content">
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
                          {server.matchReason && (
                            <span>{server.matchReason}</span>
                          )}
                          <span title="Process start time">
                            started {server.startedAt}
                          </span>
                        </div>
                        <div
                          className="server-command mono"
                          title={server.command}
                        >
                          {server.command}
                        </div>
                      </div>
                    </details>
                  </div>

                  {hasActions && (
                    <div className="server-actions">
                      {canAdd && (
                        <Button
                          className="server-action server-action-add"
                          size="1"
                          variant="solid"
                          onClick={() => onRegisterPath(server.projectRoot!)}
                          aria-label={`Add ${server.displayName} to Harbor`}
                        >
                          <PlusIcon /> <span>Add to Harbor</span>
                        </Button>
                      )}
                      {canViewApp && (
                        <Button
                          className="server-action server-action-view"
                          size="1"
                          variant="soft"
                          onClick={() => onOpenApp(server.matchedApp!)}
                          aria-label={`View ${server.matchedApp} in Harbor`}
                        >
                          View project
                        </Button>
                      )}
                      {canOpen && (
                        <Button
                          className="server-action server-action-open"
                          size="1"
                          variant="soft"
                          onClick={() => openServer(server)}
                          aria-label={`Open ${server.displayName} on port ${server.port}`}
                        >
                          <ExternalLinkIcon /> <span>Open</span>
                        </Button>
                      )}
                      {server.safeToStop && (
                        <Button
                          className="server-action server-action-stop"
                          size="1"
                          variant="soft"
                          color="red"
                          disabled={stopping !== null}
                          onClick={() => setConfirmStop(server)}
                          aria-label={`Stop ${server.displayName} on port ${server.port}`}
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
                  )}
                </article>
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
            Harbor will stop the isolated process group that owns port{" "}
            <Code>{confirmStop?.port}</Code>. Other listeners in the same group
            may stop too. Harbor re-checks the PID and start time first, and
            refuses to stop shells, terminals, IDEs, or coding agents.
          </>
        }
        confirmLabel="Stop server"
        danger
        onConfirm={() => confirmStop && stop(confirmStop)}
      />
    </>
  );
}
