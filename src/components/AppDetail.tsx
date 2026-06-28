import { useMemo, useState } from "react";
import { Button, Code, Dialog, Flex, Select, Spinner, Tooltip } from "@radix-ui/themes";
import {
  CopyIcon,
  ExternalLinkIcon,
  MagicWandIcon,
  Pencil1Icon,
  PlayIcon,
  Share1Icon,
  StopIcon,
  TrashIcon,
} from "@radix-ui/react-icons";
import { AnimatePresence, motion } from "framer-motion";
import type { AppListItem, AppRunSnapshot, LogLine, ServiceRun } from "../types";
import { api } from "../api";
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
    profiles.includes("default") ? "default" : profiles[0] ?? "default",
  );
  const [busy, setBusy] = useState<null | "start" | "stop">(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [confirmStop, setConfirmStop] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [fixOpen, setFixOpen] = useState(false);
  const [fixBusy, setFixBusy] = useState(false);
  const [fixResult, setFixResult] = useState<
    { agent?: string; text: string; copied?: boolean } | null
  >(null);

  const running = run?.running ?? false;
  const runByName = useMemo(() => {
    const m: Record<string, ServiceRun> = {};
    run?.services.forEach((s) => (m[s.name] = s));
    return m;
  }, [run]);

  const profileServices = cfg.profiles[profile] ?? cfg.services.map((s) => s.name);
  const activeServices = (
    running ? run!.services.map((s) => s.name) : profileServices
  ).filter((n, i, a) => a.indexOf(n) === i);

  async function act(kind: "start" | "stop", fn: () => Promise<unknown>) {
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
                  disabled={busy !== null}
                  onClick={() => act("start", () => api.startApp(cfg.name, profile))}
                >
                  <PlayIcon /> {busy === "start" ? "Starting…" : "Start"}
                </button>
              </motion.div>
            )}
          </AnimatePresence>

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
          {activeServices.map((name) => {
            const sc = cfg.services.find((s) => s.name === name);
            const sr = runByName[name];
            const status = sr?.status ?? "stopped";
            const errored =
              (status === "exited" && sr?.exitCode != null && sr.exitCode !== 0) ||
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
                  <StatusBadge status={status} />
                </div>
                <div className="svc-cmd">
                  {sr?.resolvedCommand ?? sc?.command ?? ""}
                </div>
                <div className="svc-meta">
                  {sr?.port != null && (
                    <span>
                      port <b>{sr.port}</b>
                    </span>
                  )}
                  {sr?.pid != null && <span>pid {sr.pid}</span>}
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
        title={`Stop ${cfg.name}?`}
        body={
          <>
            This sends <Code>SIGTERM</Code> then <Code>SIGKILL</Code> to the whole
            process tree.
          </>
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
            <Flex align="center" gap="3" style={{ padding: "20px 4px", color: "var(--text-2)" }}>
              <Spinner /> Asking your AI agent to diagnose the error…
            </Flex>
          ) : fixResult ? (
            <>
              {fixResult.copied && (
                <Dialog.Description size="2" color="gray" mb="2">
                  No Claude or Codex CLI was found locally. A tailored prompt was
                  copied to your clipboard — paste it into Claude or Codex (with
                  Harbor connected, it can read the logs over MCP).
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
