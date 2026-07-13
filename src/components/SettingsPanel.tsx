import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  Button,
  Callout,
  Code,
  Heading,
  Spinner,
  Text,
} from "@radix-ui/themes";
import {
  CheckCircledIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CopyIcon,
  EyeClosedIcon,
  EyeOpenIcon,
  InfoCircledIcon,
} from "@radix-ui/react-icons";
import { api } from "../api";
import type { AgentStatus, McpInfo } from "../types";

function CopyButton({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <Button
      size="1"
      variant="soft"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
        } catch {
          /* webview may block */
        }
        setDone(true);
        setTimeout(() => setDone(false), 1200);
      }}
    >
      {done ? <CheckIcon /> : <CopyIcon />} {done ? "Copied" : label}
    </Button>
  );
}

function ConnectCard({
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
  return (
    <div className="field" style={{ marginBottom: 12 }}>
      <div className="row" style={{ justifyContent: "space-between", gap: 10 }}>
        <div style={{ minWidth: 0 }}>
          <Text weight="bold" size="3" as="div">
            {title}
          </Text>
          <Text size="1" color="gray" as="div">
            {connected ? connectedHint : subtitle}
          </Text>
        </div>
        {connected ? (
          <span className="row" style={{ gap: 12, flex: "none" }}>
            <span
              className="row"
              style={{ gap: 5, color: "var(--ok)", whiteSpace: "nowrap" }}
            >
              <CheckCircledIcon width={16} height={16} />
              <Text size="2" weight="medium" style={{ color: "var(--ok)" }}>
                Connected
              </Text>
            </span>
            <Button
              size="1"
              variant="soft"
              color="gray"
              disabled={busy}
              onClick={onConnect}
            >
              {busy ? <Spinner size="1" /> : "Update"}
            </Button>
          </span>
        ) : available ? (
          <Button disabled={busy} onClick={onConnect} style={{ flex: "none" }}>
            {busy ? <Spinner size="1" /> : null} {connectLabel}
          </Button>
        ) : (
          <Text size="1" color="gray" style={{ flex: "none" }}>
            {unavailableNote}
          </Text>
        )}
      </div>
      {!connected && !available && (
        <div style={{ marginTop: 10 }}>{fallback}</div>
      )}
    </div>
  );
}

export function SettingsPanel({
  onAgentsChanged,
}: {
  onAgentsChanged?: () => void;
}) {
  const [info, setInfo] = useState<McpInfo | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [busy, setBusy] = useState<"code" | "desktop" | "codex" | null>(null);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [showManual, setShowManual] = useState(false);
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

  async function connect(which: "code" | "desktop" | "codex") {
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
      <div className="settings">
        <div className="settings-inner">
          <Heading size="5" mb="2">
            Connect your AI agents
          </Heading>
          {infoError ? (
            <Callout.Root color="tomato" variant="surface" size="1">
              <Callout.Icon>
                <InfoCircledIcon />
              </Callout.Icon>
              <Callout.Text>
                Could not load MCP status: {infoError}
              </Callout.Text>
            </Callout.Root>
          ) : (
            <Text size="2" color="gray">
              Loading Harbor MCP status…
            </Text>
          )}
        </div>
      </div>
    );
  }
  const token = reveal ? info.token : "•".repeat(40);
  const mcpHealthy = info.healthy && !infoError;
  const codexToml = `[mcp_servers.harbor]\nurl = "${info.url}"\nhttp_headers = { Authorization = "Bearer ${info.token}" }\ndefault_tools_approval_mode = "writes"`;

  return (
    <div className="settings">
      <div className="settings-inner">
        <Heading size="5" mb="1" style={{ letterSpacing: "-0.02em" }}>
          Connect your AI agents
        </Heading>
        <Text size="2" color="gray">
          Give Claude or Codex one safe control plane for local projects. One
          click:
        </Text>

        <div className="row" style={{ margin: "14px 0 16px", gap: 10 }}>
          <span className="chip" data-tone={mcpHealthy ? "ok" : "danger"}>
            ● {mcpHealthy ? "serving" : "offline"}
          </span>
          <Code variant="ghost" className="mono">
            {info.url}
          </Code>
        </div>

        {msg && (
          <Callout.Root
            color={msg.ok ? "green" : "tomato"}
            variant="surface"
            mb="3"
            size="1"
          >
            <Callout.Icon>
              {msg.ok ? <CheckCircledIcon /> : <InfoCircledIcon />}
            </Callout.Icon>
            <Callout.Text className="mono" style={{ fontSize: 12 }}>
              {msg.text}
            </Callout.Text>
          </Callout.Root>
        )}

        {statusError && (
          <Callout.Root color="tomato" variant="surface" mb="3" size="1">
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

        {statusLoading && !status ? (
          <div className="row" style={{ margin: "12px 0 16px", gap: 8 }}>
            <Spinner size="1" />
            <Text size="2" color="gray">
              Detecting installed AI clients…
            </Text>
          </div>
        ) : status ? (
          <>
            <ConnectCard
              title="Claude Code"
              subtitle="Add Harbor to the Claude CLI (user scope)."
              connected={status?.codeConnected ?? false}
              available={status?.codeCli ?? false}
              unavailableNote="CLI not found"
              connectedHint="Connected · run /mcp in Claude Code to use it."
              connectLabel="Connect"
              busy={busy === "code"}
              onConnect={() => connect("code")}
              fallback={
                <>
                  <Text size="1" color="gray" as="div" mb="1">
                    The <Code>claude</Code> CLI wasn't found. Run this in your
                    terminal:
                  </Text>
                  <div
                    className="row"
                    style={{ gap: 8, alignItems: "flex-start" }}
                  >
                    <div className="code-block" style={{ flex: 1 }}>
                      {info.claudeAddCommand}
                    </div>
                    <CopyButton text={info.claudeAddCommand} label="Copy" />
                  </div>
                </>
              }
            />

            <ConnectCard
              title="Claude Desktop"
              subtitle="Add Harbor to claude_desktop_config.json."
              connected={status?.desktopConnected ?? false}
              available={status?.desktopInstalled ?? false}
              unavailableNote="Not detected"
              connectedHint="Added · restart Claude Desktop to use it."
              connectLabel="Add to Claude Desktop"
              busy={busy === "desktop"}
              onConnect={() => connect("desktop")}
              fallback={
                <>
                  <Text size="1" color="gray" as="div" mb="1">
                    Claude Desktop wasn't detected. Add this to its config
                    manually:
                  </Text>
                  <div
                    className="row"
                    style={{ justifyContent: "flex-end", marginBottom: 6 }}
                  >
                    <CopyButton text={info.desktopJson} label="Copy JSON" />
                  </div>
                  <div className="code-block">{info.desktopJson}</div>
                </>
              }
            />

            <ConnectCard
              title="Codex"
              subtitle="Add Harbor to ~/.codex/config.toml."
              connected={status?.codexConnected ?? false}
              available={status?.codexInstalled ?? false}
              unavailableNote="Not detected"
              connectedHint="Added · restart Codex to use it."
              connectLabel="Connect Codex"
              busy={busy === "codex"}
              onConnect={() => connect("codex")}
              fallback={
                <>
                  <Text size="1" color="gray" as="div" mb="1">
                    Codex wasn't detected. Add this to{" "}
                    <Code>~/.codex/config.toml</Code>:
                  </Text>
                  <div
                    className="row"
                    style={{ justifyContent: "flex-end", marginBottom: 6 }}
                  >
                    <CopyButton text={codexToml} label="Copy TOML" />
                  </div>
                  <div className="code-block">{codexToml}</div>
                </>
              }
            />
          </>
        ) : null}

        <button
          className="foot-btn"
          style={{ width: "auto", padding: "6px 8px", marginTop: 4 }}
          onClick={() => setShowManual((s) => !s)}
        >
          {showManual ? <ChevronDownIcon /> : <ChevronRightIcon />} Manual setup
          & token
        </button>

        {showManual && (
          <div style={{ marginTop: 8 }}>
            <div className="field">
              <div className="field-label">
                Bearer token
                <span className="row" style={{ gap: 4 }}>
                  <Button
                    size="1"
                    variant="ghost"
                    onClick={() => setReveal((r) => !r)}
                  >
                    {reveal ? <EyeClosedIcon /> : <EyeOpenIcon />}
                  </Button>
                  <CopyButton text={info.token} label="Copy" />
                </span>
              </div>
              <div className="code-block">{token}</div>
            </div>
            <div className="field">
              <div className="field-label">
                Claude Code command
                <CopyButton text={info.claudeAddCommand} label="Copy" />
              </div>
              <div className="code-block">{info.claudeAddCommand}</div>
            </div>
            <div className="field">
              <div className="field-label">
                Claude Desktop JSON
                <CopyButton text={info.desktopJson} label="Copy" />
              </div>
              <div className="code-block">{info.desktopJson}</div>
            </div>
          </div>
        )}

        <Callout.Root color="gray" variant="surface" mt="2" size="1">
          <Callout.Icon>
            <InfoCircledIcon />
          </Callout.Icon>
          <Callout.Text>
            Harbor MCP v{info.version} is bound to 127.0.0.1 only and guarded by
            this token. After connecting, ask your agent to{" "}
            <Code>list_local_servers</Code> before starting a project.
          </Callout.Text>
        </Callout.Root>
      </div>
    </div>
  );
}
