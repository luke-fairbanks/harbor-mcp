import { useMemo, useState } from "react";
import {
  Button,
  Code,
  Dialog,
  DropdownMenu,
  Flex,
  Select,
  Spinner,
  Switch,
  Tooltip,
} from "@radix-ui/themes";
import {
  CopyIcon,
  DotsHorizontalIcon,
  ExternalLinkIcon,
  MagicWandIcon,
  Pencil1Icon,
  PlayIcon,
  ReloadIcon,
  Share1Icon,
  StopIcon,
  TrashIcon,
} from "@radix-ui/react-icons";
import { AnimatePresence, motion } from "framer-motion";
import type {
  AppListItem,
  AppRunSnapshot,
  LogLine,
  ServiceRun,
} from "../types";
import { api, formatBytes } from "../api";
import { aggregateStatus, StatusBadge, StatusDot } from "./StatusDot";
import { LogPane } from "./LogPane";
import { ConfigEditor } from "./ConfigEditor";
import { ConfirmDialog } from "./ConfirmDialog";
import { startWindowDrag } from "../titlebar";
import { ProjectGlyph } from "./icons";

export function AppDetail({
  item,
  run,
  logs,
  onChanged,
  onRemoved,
}: {
  item: AppListItem;
  run?: AppRunSnapshot;
  logs: LogLine[];
  onChanged: () => void;
  onRemoved: () => void;
}) {
  const cfg = item.config;
  const profiles = Object.keys(cfg.profiles);
  const [profile, setProfile] = useState(
    profiles.includes("default") ? "default" : (profiles[0] ?? "default"),
  );
  const [busy, setBusy] = useState<
    null | "start" | "stop" | "restart" | "config" | "approve"
  >(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [confirmStop, setConfirmStop] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [fixOpen, setFixOpen] = useState(false);
  const [fixBusy, setFixBusy] = useState(false);
  const [fixResult, setFixResult] = useState<{
    agent?: string;
    text: string;
    copied?: boolean;
  } | null>(null);

  const running = run?.running ?? false;
  const hasExternal = run?.services.some((s) => s.external) ?? false;
  const runByName = useMemo(() => {
    const m: Record<string, ServiceRun> = {};
    run?.services.forEach((s) => (m[s.name] = s));
    return m;
  }, [run]);

  const profileServices =
    cfg.profiles[profile] ?? cfg.services.map((s) => s.name);
  const activeServices = (
    running ? run!.services.map((s) => s.name) : profileServices
  ).filter((n, i, a) => a.indexOf(n) === i);
  const projectStatus = aggregateStatus(run);
  const readyCount =
    run?.services.filter((service) => service.status === "ready").length ?? 0;
  const serviceCount = running
    ? (run?.services.length ?? 0)
    : activeServices.length;
  const healthSummary = running
    ? `${readyCount} of ${serviceCount} services ready`
    : `${serviceCount} service${serviceCount === 1 ? "" : "s"} configured`;

  async function act(
    kind: "start" | "stop" | "restart",
    fn: () => Promise<unknown>,
  ) {
    setBusy(kind);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      onChanged();
    }
  }

  async function toggleAutoRestart(v: boolean) {
    setBusy("config");
    setError(null);
    try {
      await api.updateApp(cfg.name, { ...cfg, autoRestart: v });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      onChanged();
    }
  }

  async function approveCommands() {
    setBusy("approve");
    setError(null);
    try {
      await api.approveApp(cfg.name, cfg);
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function doExport() {
    try {
      const p = await api.exportApp(cfg.name);
      setNote(`Exported → ${p}`);
      setTimeout(() => setNote(null), 3500);
    } catch (e) {
      setError(String(e));
    }
  }

  function lastErrorLine(service: string): string | null {
    const svc = logs.filter((l) => l.service === service && l.line.trim());
    const errs = svc.filter((l) => l.stream === "stderr");
    const pick = (errs.length ? errs : svc).slice(-1)[0];
    return pick?.line ?? null;
  }

  async function fixWithAI(service: string) {
    setFixOpen(true);
    setFixBusy(true);
    setFixResult(null);
    try {
      const r = await api.runFix(cfg.name, service);
      setFixResult({ agent: r.agent, text: r.response });
    } catch {
      // No agent CLI found — copy a ready-to-paste prompt instead.
      let prompt = "";
      try {
        prompt = await api.fixPrompt(cfg.name, service);
        await navigator.clipboard.writeText(prompt);
      } catch {
        /* ignore */
      }
      setFixResult({ text: prompt, copied: true });
    } finally {
      setFixBusy(false);
    }
  }

  return (
    <>
      <div
        className="detail-head app-detail-head"
        onMouseDown={startWindowDrag}
      >
        <div className="app-title-cluster">
          <div className="page-eyebrow">
            <span>Project</span>
            <StatusBadge status={projectStatus} context="Project" />
          </div>
          <div className="app-title-row">
            <ProjectGlyph name={cfg.name} />
            <div className="app-title-copy">
              <h1 className="detail-title">{cfg.name}</h1>
              <div className="project-health-summary">{healthSummary}</div>
            </div>
          </div>
          <div className="project-context" data-no-drag>
            <span className="detail-sub" title={cfg.root}>
              {cfg.root}
            </span>
            <label className="auto-restart-toggle">
              <Switch
                size="1"
                checked={cfg.autoRestart ?? false}
                onCheckedChange={toggleAutoRestart}
                disabled={busy !== null}
              />
              <Tooltip content="If a service Harbor started exits unexpectedly, Harbor restarts it with backoff. It never affects servers started outside Harbor.">
                <span>Auto-restart</span>
              </Tooltip>
            </label>
          </div>
        </div>

        <div className="app-header-actions" data-no-drag>
          {!running && profiles.length > 1 && (
            <label className="profile-control">
              <span>Profile</span>
              <Select.Root value={profile} onValueChange={setProfile} size="2">
                <Select.Trigger variant="surface" />
                <Select.Content>
                  {profiles.map((p) => (
                    <Select.Item key={p} value={p}>
                      {p}
                    </Select.Item>
                  ))}
                </Select.Content>
              </Select.Root>
            </label>
          )}

          <AnimatePresence mode="wait" initial={false}>
            {running ? (
              <motion.div
                key="stop"
                initial={{ opacity: 0, scale: 0.97 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.97 }}
                transition={{ duration: 0.12 }}
              >
                <button
                  className="run-btn"
                  data-kind="stop"
                  disabled={busy !== null}
                  onClick={() => setConfirmStop(true)}
                >
                  <StopIcon /> {busy === "stop" ? "Stopping…" : "Stop project"}
                </button>
              </motion.div>
            ) : (
              <motion.div
                key="start"
                initial={{ opacity: 0, scale: 0.97 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.97 }}
                transition={{ duration: 0.12 }}
              >
                <button
                  className="run-btn"
                  disabled={busy !== null || cfg.trusted === false}
                  onClick={() =>
                    act("start", () => api.startApp(cfg.name, profile))
                  }
                >
                  <PlayIcon />{" "}
                  {busy === "start" ? "Starting…" : "Start project"}
                </button>
              </motion.div>
            )}
          </AnimatePresence>

          {running && (
            <>
              <Tooltip content="Restart project">
                <button
                  className="icon-btn"
                  aria-label={`Restart ${cfg.name}`}
                  disabled={busy !== null}
                  onClick={() => act("restart", () => api.restartApp(cfg.name))}
                >
                  <ReloadIcon
                    className={busy === "restart" ? "spin" : undefined}
                  />
                </button>
              </Tooltip>
              <Tooltip content="Open in browser">
                <button
                  className="icon-btn"
                  aria-label={`Open ${cfg.name} in a browser`}
                  onClick={() => api.openApp(cfg.name)}
                >
                  <ExternalLinkIcon />
                </button>
              </Tooltip>
            </>
          )}

          <DropdownMenu.Root>
            <DropdownMenu.Trigger>
              <button
                className="icon-btn"
                aria-label={`More actions for ${cfg.name}`}
              >
                <DotsHorizontalIcon />
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Content align="end" size="1">
              <DropdownMenu.Item
                disabled={running}
                onSelect={() => setEditing(true)}
              >
                <Pencil1Icon /> Edit configuration
              </DropdownMenu.Item>
              <DropdownMenu.Item onSelect={doExport}>
                <Share1Icon /> Export harbor.json
              </DropdownMenu.Item>
              <DropdownMenu.Separator />
              <DropdownMenu.Item
                color="red"
                disabled={running}
                onSelect={() => setConfirmRemove(true)}
              >
                <TrashIcon /> Remove project
              </DropdownMenu.Item>
            </DropdownMenu.Content>
          </DropdownMenu.Root>
        </div>
      </div>

      <div className="detail-body">
        {cfg.trusted === false && (
          <div className="trust-banner">
            <div>
              <div className="trust-title">
                Review required before this app can run
              </div>
              <div className="trust-copy">
                An AI agent registered or changed this config. Check the
                commands below, then approve them once. Harbor will not execute
                an unreviewed config.
              </div>
            </div>
            <Button
              size="2"
              variant="solid"
              disabled={busy !== null}
              onClick={approveCommands}
            >
              {busy === "approve" ? <Spinner size="1" /> : null}
              Approve commands
            </Button>
          </div>
        )}
        {(error || note) && (
          <div
            className="async-notice mono"
            data-tone={error ? "danger" : "neutral"}
            role={error ? "alert" : "status"}
          >
            {error ?? note}
          </div>
        )}

        {run && run.portPlan.length > 0 && (
          <div className="port-plan">
            <span className="section-label">Port plan</span>
            {run.portPlan.map((p) => (
              <span
                key={p.service}
                className="chip"
                data-tone={p.note ? "warn" : "accent"}
              >
                {p.service} → {p.resolved}
                {p.note ? ` · ${p.note}` : ""}
              </span>
            ))}
          </div>
        )}

        <div className="svc-grid">
          {(cfg.trusted === false
            ? cfg.services.map((service) => service.name)
            : activeServices
          ).map((name) => {
            const sc = cfg.services.find((s) => s.name === name);
            const sr = runByName[name];
            const status = sr?.status ?? "stopped";
            const errored =
              (status === "exited" &&
                sr?.exitCode != null &&
                sr.exitCode !== 0) ||
              status === "unhealthy";
            const errLine = errored ? lastErrorLine(name) : null;
            return (
              <motion.div
                className="svc-card"
                key={name}
                data-errored={errored}
                data-status={status}
                initial={{ opacity: 0, y: 4 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.18 }}
              >
                <div className="svc-card-top">
                  <span className="svc-name">
                    <StatusDot status={status} />
                    {name}
                  </span>
                  <span className="svc-status-group">
                    {sr?.external ? (
                      <Tooltip content="Started outside Harbor (e.g. from a terminal). Harbor matched this process to the app by its port and project folder. Open and Stop work, but live logs aren't captured — Stop here, then Start to run it under Harbor with logs.">
                        <span className="external-tag">external</span>
                      </Tooltip>
                    ) : sr?.adopted ? (
                      <Tooltip content="Recovered from a previous Harbor session — Harbor holds its process and port, but live logs aren't available. Stop & Start to recapture output.">
                        <span className="adopted-tag">adopted</span>
                      </Tooltip>
                    ) : null}
                    <StatusBadge status={status} />
                  </span>
                </div>
                {cfg.trusted === false ? (
                  <>
                    <div className="svc-cmd trust-command">
                      {sc?.command ?? ""}
                    </div>
                    {sc && (
                      <div className="trust-command-details mono">
                        <span>cwd: {sc.cwd || "."}</span>
                        {sc.port != null && <span>port: {sc.port}</span>}
                        {Object.entries(sc.env).map(([key, value]) => (
                          <span key={key}>
                            {key}={value}
                          </span>
                        ))}
                      </div>
                    )}
                  </>
                ) : null}
                <div className="svc-meta">
                  {sr?.port != null &&
                    (status === "ready" ? (
                      <button
                        className="svc-url"
                        title="Open in browser"
                        onClick={() =>
                          api.openUrl(`http://localhost:${sr.port}`)
                        }
                      >
                        localhost:{sr.port}
                        <ExternalLinkIcon width={11} height={11} />
                      </button>
                    ) : (
                      <span>
                        port <b>{sr.port}</b>
                      </span>
                    ))}
                  {sr?.pid != null && <span>pid {sr.pid}</span>}
                  {status !== "stopped" &&
                    status !== "exited" &&
                    sr?.cpu != null &&
                    sr?.memBytes != null && (
                      <span
                        className="svc-res"
                        title="Recent CPU% and resident memory for the whole process group"
                      >
                        {sr.cpu >= 10 ? sr.cpu.toFixed(0) : sr.cpu.toFixed(1)}%
                        · {formatBytes(sr.memBytes)}
                      </span>
                    )}
                  {sr?.exitCode != null && (
                    <span style={{ color: "var(--danger)" }}>
                      exit {sr.exitCode}
                    </span>
                  )}
                </div>
                {cfg.trusted !== false && (
                  <details className="svc-details">
                    <summary>Service details</summary>
                    <div className="svc-details-content">
                      <div className="svc-cmd">
                        {sr?.resolvedCommand ?? sc?.command ?? ""}
                      </div>
                      <div className="svc-tech-meta mono">
                        <span>cwd: {sc?.cwd || "."}</span>
                        {sc?.dependsOn && sc.dependsOn.length > 0 && (
                          <span>after: {sc.dependsOn.join(", ")}</span>
                        )}
                      </div>
                    </div>
                  </details>
                )}
                {errored && (
                  <>
                    {errLine && <div className="svc-error">{errLine}</div>}
                    <div className="svc-actions">
                      <button
                        className="fix-btn"
                        disabled={fixBusy}
                        onClick={() => fixWithAI(name)}
                      >
                        <span className="accent">
                          <MagicWandIcon />
                        </span>{" "}
                        Fix with AI
                      </button>
                    </div>
                  </>
                )}
              </motion.div>
            );
          })}
        </div>

        <LogPane logs={logs} services={activeServices} running={running} />
      </div>

      {editing && (
        <ConfigEditor
          open
          onOpenChange={(v) => !v && setEditing(false)}
          app={cfg}
          onSaved={onChanged}
        />
      )}

      <ConfirmDialog
        open={confirmStop}
        onOpenChange={setConfirmStop}
        title={
          hasExternal
            ? `Stop ${cfg.name}? (started outside Harbor)`
            : `Stop ${cfg.name}?`
        }
        body={
          hasExternal ? (
            <>
              This server was started <b>outside Harbor</b>. Stopping sends{" "}
              <Code>SIGTERM</Code> then <Code>SIGKILL</Code> to its entire
              process group — the terminal command that launched it and every
              child process will be terminated.
            </>
          ) : (
            <>
              This sends <Code>SIGTERM</Code> then <Code>SIGKILL</Code> to the
              whole process tree.
            </>
          )
        }
        confirmLabel="Stop"
        danger
        onConfirm={() => act("stop", () => api.stopApp(cfg.name))}
      />
      <ConfirmDialog
        open={confirmRemove}
        onOpenChange={setConfirmRemove}
        title={`Remove ${cfg.name}?`}
        body="This removes it from Harbor's registry. Your project's files are not touched."
        confirmLabel="Remove"
        danger
        onConfirm={async () => {
          await api.removeApp(cfg.name);
          onRemoved();
        }}
      />

      <Dialog.Root open={fixOpen} onOpenChange={setFixOpen}>
        <Dialog.Content maxWidth="660px">
          <Dialog.Title>
            {fixBusy
              ? "Diagnosing…"
              : fixResult?.copied
                ? "Fix prompt copied"
                : `${fixResult?.agent ?? "AI"} suggests`}
          </Dialog.Title>
          {fixBusy ? (
            <Flex
              align="center"
              gap="3"
              style={{ padding: "20px 4px", color: "var(--text-2)" }}
            >
              <Spinner /> Asking your AI agent to diagnose the error…
            </Flex>
          ) : fixResult ? (
            <>
              {fixResult.copied && (
                <Dialog.Description size="2" color="gray" mb="2">
                  No Claude or Codex CLI was found locally. A tailored prompt
                  was copied to your clipboard — paste it into Claude or Codex
                  (with Harbor connected, it can read the logs over MCP).
                </Dialog.Description>
              )}
              <div className="fix-response">{fixResult.text}</div>
              <Flex gap="3" mt="3" justify="end">
                <Button
                  variant="soft"
                  color="gray"
                  onClick={async () => {
                    try {
                      await navigator.clipboard.writeText(fixResult.text);
                    } catch {
                      /* ignore */
                    }
                  }}
                >
                  <CopyIcon /> Copy
                </Button>
                <Dialog.Close>
                  <Button>Done</Button>
                </Dialog.Close>
              </Flex>
            </>
          ) : null}
        </Dialog.Content>
      </Dialog.Root>
    </>
  );
}
