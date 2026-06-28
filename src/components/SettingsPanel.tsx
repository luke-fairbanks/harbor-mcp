import { useEffect, useState } from "react";
import { Button, Callout, Code, Heading, Text } from "@radix-ui/themes";
import {
  CheckIcon,
  CopyIcon,
  EyeClosedIcon,
  EyeOpenIcon,
  InfoCircledIcon,
} from "@radix-ui/react-icons";
import { api } from "../api";
import type { McpInfo } from "../types";

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
          /* webview may block; ignore */
        }
        setDone(true);
        setTimeout(() => setDone(false), 1200);
      }}
    >
      {done ? <CheckIcon /> : <CopyIcon />} {done ? "Copied" : label}
    </Button>
  );
}

export function SettingsPanel() {
  const [info, setInfo] = useState<McpInfo | null>(null);
  const [reveal, setReveal] = useState(false);

  useEffect(() => {
    api.mcpInfo().then(setInfo);
  }, []);

  if (!info) return null;
  const token = reveal ? info.token : "•".repeat(40);

  return (
    <div className="settings">
      <div className="settings-inner">
        <Heading size="5" mb="1" style={{ letterSpacing: "-0.02em" }}>
          Connect your Claude
        </Heading>
        <Text size="2" color="gray">
          Harbor hosts a local MCP server so your Claude can discover, configure,
          and drive your apps.
        </Text>

        <div className="row" style={{ margin: "16px 0 18px", gap: 10 }}>
          <span className="chip" data-tone="ok">
            ● serving
          </span>
          <Code variant="ghost" className="mono">
            {info.url}
          </Code>
        </div>

        <div className="field">
          <div className="field-label">
            Bearer token
            <span className="row" style={{ gap: 4 }}>
              <Button size="1" variant="ghost" onClick={() => setReveal((r) => !r)}>
                {reveal ? <EyeClosedIcon /> : <EyeOpenIcon />}
              </Button>
              <CopyButton text={info.token} label="Copy" />
            </span>
          </div>
          <div className="code-block">{token}</div>
        </div>

        <div className="field">
          <div className="field-label">
            Claude Code
            <CopyButton text={info.claudeAddCommand} label="Copy command" />
          </div>
          <div className="code-block">{info.claudeAddCommand}</div>
        </div>

        <div className="field">
          <div className="field-label">
            Claude Desktop · claude_desktop_config.json
            <CopyButton text={info.desktopJson} label="Copy JSON" />
          </div>
          <div className="code-block">{info.desktopJson}</div>
        </div>

        <Callout.Root color="gray" variant="surface" mt="2">
          <Callout.Icon>
            <InfoCircledIcon />
          </Callout.Icon>
          <Callout.Text>
            Bound to 127.0.0.1 only and guarded by this token. After adding it, ask
            your Claude to <Code>detect_app</Code> a folder, then{" "}
            <Code>start_app</Code> it.
          </Callout.Text>
        </Callout.Root>
      </div>
    </div>
  );
}
