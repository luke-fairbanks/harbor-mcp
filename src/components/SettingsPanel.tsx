import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Button, Callout, Code, Spinner } from "@radix-ui/themes";
import {
  CheckCircledIcon,
  CheckIcon,
  CopyIcon,
  EyeClosedIcon,
  EyeOpenIcon,
  InfoCircledIcon,
} from "@radix-ui/react-icons";
import { api } from "../api";
import type { AgentStatus, McpInfo } from "../types";

type AgentKind = "code" | "desktop" | "codex";

function CopyButton({ text, label }: { text: string; label: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const copied = state === "copied";
  const failed = state === "failed";

  return (
    <Button
      className="connections-copy-button"
      size="1"
      variant="soft"
      color={failed ? "red" : undefined}
      aria-label={
        copied ? `${label} copied` : failed ? `${label} failed` : label
      }
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setState("copied");
        } catch {
          setState("failed");
        }
        window.setTimeout(() => setState("idle"), 1600);
      }}
    >
      {copied ? <CheckIcon /> : failed ? <InfoCircledIcon /> : <CopyIcon />}
      <span>{copied ? "Copied" : failed ? "Copy failed" : label}</span>
    </Button>
  );
}

function ConnectionsHeader() {
  return (
    <header className="connections-header">
      <p className="connections-eyebrow">AI connections</p>
      <h1 className="connections-title">One bridge. Every agent.</h1>
      <p className="connections-intro">
        Give the tools you already use one safe, local control plane for every
        project in Harbor.
      </p>
    </header>
  );
}

function ConnectCard({
  kind,
  title,
  subtitle,
  connected,
  available,
  unavailableNote,
  connectedHint,
  connectLabel,
  busy,
  onConnect,
  fallback,
}: {
  kind: AgentKind;
  title: string;
  subtitle: string;
  connected: boolean;
  available: boolean;
  unavailableNote: string;
  connectedHint: string;
  connectLabel: string;
  busy: boolean;
  onConnect: () => void;
  fallback: ReactNode;
}) {
  const subtitleId = `connections-${kind}-description`;

  return (
    <article
      className="connections-card"
      data-agent={kind}
      data-connected={connected || undefined}
      data-available={available || undefined}
      aria-labelledby={`connections-${kind}-title`}
      aria-describedby={subtitleId}
      aria-busy={busy}
    >
      <div className="connections-card-head">
        <div className="connections-client-mark" data-agent={kind} aria-hidden>
          {kind === "code" ? ">_" : kind === "desktop" ? "C" : "CX"}
        </div>
        <div className="connections-card-copy">
          <h3
            className="connections-card-title"
            id={`connections-${kind}-title`}
          >
            {title}
          </h3>
          <p className="connections-card-subtitle" id={subtitleId}>
            {connected ? connectedHint : subtitle}
          </p>
        </div>
      </div>

      <div className="connections-card-status-row">
        {connected ? (
          <span className="connections-card-status" data-tone="connected">
            <CheckCircledIcon aria-hidden />
            Connected
          </span>
        ) : available ? (
          <span className="connections-card-status" data-tone="available">
            <span className="connections-status-dot" aria-hidden />
            Ready to connect
          </span>
        ) : (
          <span className="connections-card-status" data-tone="unavailable">
            <span className="connections-status-dot" aria-hidden />
            {unavailableNote}
          </span>
        )}
      </div>

      {!connected && !available && (
        <div className="connections-card-fallback">{fallback}</div>
      )}

      <div className="connections-card-action">
        {connected ? (
          <Button
            className="connections-action-button"
            size="2"
            variant="soft"
            color="gray"
            disabled={busy}
            onClick={onConnect}
            aria-label={`Update ${title} connection`}
          >
            {busy ? <Spinner size="1" /> : null}
            {busy ? "Updating…" : "Update connection"}
          </Button>
        ) : available ? (
          <Button
            className="connections-action-button"
            size="2"
            disabled={busy}
            onClick={onConnect}
            aria-label={`${connectLabel} to Harbor`}
          >
            {busy ? <Spinner size="1" /> : null}
            {busy ? "Connecting…" : connectLabel}
          </Button>
        ) : null}
      </div>
    </article>
  );
}

export function SettingsPanel({
  onAgentsChanged,
}: {
  onAgentsChanged?: () => void;
}) {
  const [info, setInfo] = useState<McpInfo | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [busy, setBusy] = useState<AgentKind | null>(null);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [reveal, setReveal] = useState(false);
  const [infoError, setInfoError] = useState<string | null>(null);
  const [statusLoading, setStatusLoading] = useState(true);
  const [statusError, setStatusError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setStatusLoading(true);
    try {
      setStatus(await api.agentsStatus());
      setStatusError(null);
    } catch (e) {
      setStatusError(String(e));
    } finally {
      setStatusLoading(false);
    }
  }, []);

  const refreshInfo = useCallback(async () => {
    try {
      setInfo(await api.mcpInfo());
      setInfoError(null);
    } catch (e) {
      setInfoError(String(e));
    }
  }, []);

  useEffect(() => {
    refreshInfo();
    refresh();
    const timer = window.setInterval(refreshInfo, 5_000);
    return () => window.clearInterval(timer);
  }, [refresh, refreshInfo]);

  async function connect(which: AgentKind) {
    setBusy(which);
    setMsg(null);
    try {
      const text =
        which === "code"
          ? await api.connectClaudeCode()
          : which === "desktop"
            ? await api.connectClaudeDesktop()
            : await api.connectCodex();
      setMsg({ ok: true, text });
      await refresh();
      onAgentsChanged?.();
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(null);
    }
  }

  if (!info) {
    return (
      <div className="settings connections-page">
        <div className="settings-inner connections-page-inner">
          <ConnectionsHeader />
          <div className="connections-loading" aria-live="polite">
            {infoError ? (
              <Callout.Root
                color="tomato"
                variant="surface"
                size="1"
                role="alert"
              >
                <Callout.Icon>
                  <InfoCircledIcon />
                </Callout.Icon>
                <Callout.Text>
                  Could not load MCP status: {infoError}
                </Callout.Text>
              </Callout.Root>
            ) : (
              <div className="connections-loading-state" role="status">
                <Spinner size="2" />
                <span>Waking the Harbor MCP bridge…</span>
              </div>
            )}
          </div>
        </div>
      </div>
    );
  }

  const token = reveal ? info.token : "•".repeat(40);
  const mcpHealthy = info.healthy && !infoError;
  const codexToml = `[mcp_servers.harbor]\nurl = "${info.url}"\nhttp_headers = { Authorization = "Bearer ${info.token}" }\ndefault_tools_approval_mode = "writes"`;

  return (
    <div className="settings connections-page">
      <div className="settings-inner connections-page-inner">
        <ConnectionsHeader />

        <section
          className="bridge-hero"
          data-healthy={mcpHealthy || undefined}
          aria-labelledby="bridge-title"
        >
          <div className="bridge-beacon" aria-hidden>
            <span className="bridge-beacon-core" />
            <span className="bridge-beacon-ring" />
          </div>
          <div className="bridge-main">
            <p className="bridge-kicker">Harbor MCP bridge</p>
            <h2 className="bridge-title" id="bridge-title">
              Local control plane
            </h2>
            <div className="bridge-endpoint">
              <Code variant="ghost" className="mono">
                {info.url}
              </Code>
              <CopyButton text={info.url} label="Copy address" />
            </div>
          </div>
          <div className="bridge-status" aria-live="polite">
            <span className="bridge-status-dot" aria-hidden />
            <span>{mcpHealthy ? "Online" : "Offline"}</span>
          </div>
          <div className="bridge-meta">
            <span>Loopback only</span>
            <span aria-hidden>·</span>
            <span>Token protected</span>
            <span aria-hidden>·</span>
            <span>v{info.version}</span>
          </div>
        </section>

        <div
          className="connections-feedback"
          aria-live="polite"
          aria-atomic="true"
        >
          {msg && (
            <Callout.Root
              color={msg.ok ? "green" : "tomato"}
              variant="surface"
              size="1"
              role={msg.ok ? "status" : "alert"}
            >
              <Callout.Icon>
                {msg.ok ? <CheckCircledIcon /> : <InfoCircledIcon />}
              </Callout.Icon>
              <Callout.Text className="mono">{msg.text}</Callout.Text>
            </Callout.Root>
          )}

          {statusError && (
            <Callout.Root
              color="tomato"
              variant="surface"
              size="1"
              role="alert"
            >
              <Callout.Icon>
                <InfoCircledIcon />
              </Callout.Icon>
              <Callout.Text>
                Could not inspect installed AI clients. {statusError}{" "}
                <Button size="1" variant="ghost" onClick={refresh}>
                  Retry
                </Button>
              </Callout.Text>
            </Callout.Root>
          )}
        </div>

        <section
          className="connections-section"
          aria-labelledby="connections-agents-title"
        >
          <div className="connections-section-header">
            <div>
              <p className="connections-eyebrow">Available connections</p>
              <h2
                className="connections-section-title"
                id="connections-agents-title"
              >
                Bring your agents aboard
              </h2>
            </div>
            <p className="connections-section-copy">
              Connect once, then let each agent discover and manage the same
              local projects.
            </p>
          </div>

          {statusLoading && !status ? (
            <div className="connections-detecting" role="status">
              <Spinner size="1" />
              <span>Detecting installed AI clients…</span>
            </div>
          ) : status ? (
            <div className="connections-grid">
              <ConnectCard
                kind="code"
                title="Claude Code"
                subtitle="Connect the Claude CLI at user scope."
                connected={status.codeConnected}
                available={status.codeCli}
                unavailableNote="CLI not found"
                connectedHint="Ready in Claude Code. Run /mcp to use Harbor."
                connectLabel="Connect Claude Code"
                busy={busy === "code"}
                onConnect={() => connect("code")}
                fallback={
                  <>
                    <p className="connections-fallback-copy">
                      Run this command in your terminal:
                    </p>
                    <div className="connections-code-row">
                      <div className="code-block connections-code-block">
                        {info.claudeAddCommand}
                      </div>
                      <CopyButton text={info.claudeAddCommand} label="Copy" />
                    </div>
                  </>
                }
              />

              <ConnectCard
                kind="desktop"
                title="Claude Desktop"
                subtitle="Add Harbor to the Claude Desktop app."
                connected={status.desktopConnected}
                available={status.desktopInstalled}
                unavailableNote="App not detected"
                connectedHint="Added. Restart Claude Desktop to use Harbor."
                connectLabel="Connect Desktop"
                busy={busy === "desktop"}
                onConnect={() => connect("desktop")}
                fallback={
                  <>
                    <p className="connections-fallback-copy">
                      Add the MCP entry to Claude Desktop manually:
                    </p>
                    <div className="connections-code-row">
                      <div className="code-block connections-code-block">
                        {info.desktopJson}
                      </div>
                      <CopyButton text={info.desktopJson} label="Copy JSON" />
                    </div>
                  </>
                }
              />

              <ConnectCard
                kind="codex"
                title="Codex"
                subtitle="Connect Harbor through your Codex config."
                connected={status.codexConnected}
                available={status.codexInstalled}
                unavailableNote="App not detected"
                connectedHint="Added. Restart Codex to use Harbor."
                connectLabel="Connect Codex"
                busy={busy === "codex"}
                onConnect={() => connect("codex")}
                fallback={
                  <>
                    <p className="connections-fallback-copy">
                      Add this entry to <Code>~/.codex/config.toml</Code>:
                    </p>
                    <div className="connections-code-row">
                      <div className="code-block connections-code-block">
                        {codexToml}
                      </div>
                      <CopyButton text={codexToml} label="Copy TOML" />
                    </div>
                  </>
                }
              />
            </div>
          ) : null}
        </section>

        <details className="connections-advanced">
          <summary className="connections-advanced-summary">
            <span>
              <span className="connections-advanced-title">Advanced setup</span>
              <span className="connections-advanced-subtitle">
                Token and manual client configuration
              </span>
            </span>
          </summary>

          <div className="connections-advanced-content">
            <div className="connections-config-grid">
              <section className="connections-config-card">
                <div className="connections-config-header">
                  <h3>Bearer token</h3>
                  <div className="connections-config-actions">
                    <Button
                      size="1"
                      variant="ghost"
                      onClick={() => setReveal((r) => !r)}
                      aria-label={
                        reveal ? "Hide bearer token" : "Reveal bearer token"
                      }
                      aria-pressed={reveal}
                    >
                      {reveal ? <EyeClosedIcon /> : <EyeOpenIcon />}
                      {reveal ? "Hide" : "Reveal"}
                    </Button>
                    <CopyButton text={info.token} label="Copy token" />
                  </div>
                </div>
                <div className="code-block connections-code-block">{token}</div>
              </section>

              <section className="connections-config-card">
                <div className="connections-config-header">
                  <h3>Claude Code command</h3>
                  <CopyButton text={info.claudeAddCommand} label="Copy" />
                </div>
                <div className="code-block connections-code-block">
                  {info.claudeAddCommand}
                </div>
              </section>

              <section className="connections-config-card">
                <div className="connections-config-header">
                  <h3>Claude Desktop JSON</h3>
                  <CopyButton text={info.desktopJson} label="Copy JSON" />
                </div>
                <div className="code-block connections-code-block">
                  {info.desktopJson}
                </div>
              </section>

              <section className="connections-config-card">
                <div className="connections-config-header">
                  <h3>Codex TOML</h3>
                  <CopyButton text={codexToml} label="Copy TOML" />
                </div>
                <div className="code-block connections-code-block">
                  {codexToml}
                </div>
              </section>
            </div>
          </div>
        </details>

        <aside className="connections-security-note" aria-label="MCP security">
          <InfoCircledIcon aria-hidden />
          <p>
            Harbor only listens on <Code>127.0.0.1</Code> and requires this
            bearer token. After connecting, ask your agent to{" "}
            <Code>list_local_servers</Code> before starting a project.
          </p>
        </aside>
      </div>
    </div>
  );
}
