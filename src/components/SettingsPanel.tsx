import { useEffect, useState } from "react";
import {
  Badge,
  Box,
  Button,
  Callout,
  Card,
  Code,
  Flex,
  Heading,
  Text,
} from "@radix-ui/themes";
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

  const tokenShown = reveal ? info.token : "•".repeat(24);

  return (
    <Box p="5" style={{ maxWidth: 760, overflowY: "auto" }} className="fill">
      <Heading size="5" mb="1">
        Connect your Claude
      </Heading>
      <Text size="2" color="gray">
        Harbor hosts a local MCP server so your Claude can discover, configure,
        and drive your apps.
      </Text>

      <Flex gap="2" align="center" mt="3" mb="4">
        <Badge color="green" variant="soft">
          ● serving
        </Badge>
        <Code variant="ghost">{info.url}</Code>
      </Flex>

      <Card mb="4">
        <Flex direction="column" gap="2">
          <Text size="1" color="gray" weight="medium">
            BEARER TOKEN
          </Text>
          <Flex align="center" gap="2">
            <Code className="mono" style={{ flex: 1 }}>
              {tokenShown}
            </Code>
            <Button size="1" variant="ghost" onClick={() => setReveal((r) => !r)}>
              {reveal ? <EyeClosedIcon /> : <EyeOpenIcon />}
            </Button>
            <CopyButton text={info.token} label="Copy" />
          </Flex>
        </Flex>
      </Card>

      <Card mb="4">
        <Flex direction="column" gap="2">
          <Flex align="center" justify="between">
            <Text size="1" color="gray" weight="medium">
              CLAUDE CODE
            </Text>
            <CopyButton text={info.claudeAddCommand} label="Copy command" />
          </Flex>
          <Code
            className="mono"
            variant="soft"
            style={{ whiteSpace: "pre-wrap", wordBreak: "break-all", padding: 10 }}
          >
            {info.claudeAddCommand}
          </Code>
        </Flex>
      </Card>

      <Card mb="4">
        <Flex direction="column" gap="2">
          <Flex align="center" justify="between">
            <Text size="1" color="gray" weight="medium">
              CLAUDE DESKTOP (claude_desktop_config.json)
            </Text>
            <CopyButton text={info.desktopJson} label="Copy JSON" />
          </Flex>
          <Code
            className="mono"
            variant="soft"
            style={{ whiteSpace: "pre-wrap", padding: 10 }}
          >
            {info.desktopJson}
          </Code>
        </Flex>
      </Card>

      <Callout.Root color="gray" variant="surface">
        <Callout.Icon>
          <InfoCircledIcon />
        </Callout.Icon>
        <Callout.Text>
          The server binds to 127.0.0.1 only and requires this token. After
          adding it, ask your Claude to <Code>detect_app</Code> a folder, then{" "}
          <Code>start_app</Code> it.
        </Callout.Text>
      </Callout.Root>
    </Box>
  );
}
