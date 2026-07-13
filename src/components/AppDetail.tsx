import { useMemo, useState } from "react";
import {
  Button,
  Code,
  Dialog,
  Flex,
  Select,
  Spinner,
  Switch,
  Tooltip,
} from "@radix-ui/themes";
import {
  CopyIcon,
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
import { StatusBadge, StatusDot } from "./StatusDot";
import { LogPane } from "./LogPane";
import { ConfigEditor } from "./ConfigEditor";
import { ConfirmDialog } from "./ConfirmDialog";
import { startWindowDrag } from "../titlebar";

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
      <div className="detail-head" onMouseDown={startWindowDrag}>
        <div style={{ minWidth: 0 }}>
          <div className="detail-title">{cfg.name}</div>
          <div className="detail-sub" data-no-drag>
            {cfg.root}
          </div>
          <label className="auto-restart-toggle" data-no-drag>
            <Switch
              size="1"
              checked={cfg.autoRestart ?? false}
              onCheckedChange={toggleAutoRestart}
              disabled={busy !== null}
            />
            <Tooltip content="If a service Harbor started exits unexpectedly, Harbor restarts it (up to 5 times, with backoff). Never affects servers started outside Harbor.">
              <span>Auto-restart on crash</span>
            </Tooltip>
          </label>
        </div>
        <div className="row" style={{ flex: "none" }}>
          {!running && profiles.length > 1 && (
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
                  <StopIcon /> {busy === "stop" ? "Stopping…" : "Stop"}
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
                  <PlayIcon /> {busy === "start" ? "Starting…" : "Start"}
                </button>
              </motion.div>
            )}
          </AnimatePresence>

          <Tooltip content="Restart">
            <button
              className="icon-btn"
              disabled={!running || busy !== null}
              onClick={() => act("restart", () => api.restartApp(cfg.name))}
            >
              <ReloadIcon className={busy === "restart" ? "spin" : undefined} />
            </button>
          </Tooltip>
          <Tooltip content="Open in browser">
            <button
              className="icon-btn"
              disabled={!running}
              onClick={() => api.openApp(cfg.name)}
            >
              <ExternalLinkIcon />
            </button>
          </Tooltip>
          <Tooltip content="Edit config">
            <button
              className="icon-btn"
              disabled={running}
              onClick={() => setEditing(true)}
            >
              <Pencil1Icon />
            </button>
          </Tooltip>
          <Tooltip content="Export harbor.json">
            <button className="icon-btn" onClick={doExport}>
              <Share1Icon />
            </button>
          </Tooltip>
          <Tooltip content={running ? "Stop before removing" : "Remove app"}>
            <button
              className="icon-btn"
              disabled={running}
              onClick={() => setConfirmRemove(true)}
            >
              <TrashIcon />
            </button>
          </Tooltip>
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
            className="mono"
            style={{
              fontSize: 12,
              color: error ? "var(--danger)" : "var(--text-2)",
            }}
          >
            {error ?? note}
          </div>
        )}

        {run && run.portPlan.length > 0 && (
          <div className="row" style={{ flexWrap: "wrap", gap: 8 }}>
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
                initial={{ opacity: 0, y: 4 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.18 }}
              >
                <div className="svc-card-top">
                  <span className="svc-name">
                    <StatusDot status={status} />
                    {name}
                  </span>
                  <span className="row" style={{ gap: 6, flex: "none" }}>
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
                <div
                  className={`svc-cmd ${cfg.trusted === false ? "trust-command" : ""}`}
                >
                  {cfg.trusted === false
                    ? (sc?.command ?? "")
                    : (sr?.resolvedCommand ?? sc?.command ?? "")}
                </div>
                {cfg.trusted === false && sc && (
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
                  {sc?.dependsOn && sc.dependsOn.length > 0 && (
                    <span>↳ {sc.dependsOn.join(", ")}</span>
                  )}
                  {sr?.exitCode != null && (
                    <span style={{ color: "var(--danger)" }}>
                      exit {sr.exitCode}
                    </span>
                  )}
                </div>
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

        <LogPane logs={logs} services={activeServices} />
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
