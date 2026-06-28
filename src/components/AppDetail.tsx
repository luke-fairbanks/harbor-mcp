import { useMemo, useState } from "react";
import {
  Badge,
  Box,
  Button,
  Card,
  Code,
  Flex,
  Heading,
  IconButton,
  Select,
  Separator,
  Text,
  Tooltip,
} from "@radix-ui/themes";
import {
  ExternalLinkIcon,
  PlayIcon,
  StopIcon,
  TrashIcon,
} from "@radix-ui/react-icons";
import type { AppListItem, AppRunSnapshot, LogLine, ServiceRun } from "../types";
import { api } from "../api";
import { STATUS_BADGE } from "./StatusDot";
import { LogPane } from "./LogPane";

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

  return (
    <Flex direction="column" className="fill" p="4" gap="3">
      {/* Header */}
      <Flex align="center" justify="between" gap="3">
        <Box>
          <Heading size="5">{cfg.name}</Heading>
          <Text size="1" color="gray" className="mono">
            {cfg.root}
          </Text>
        </Box>
        <Flex align="center" gap="2">
          {!running && profiles.length > 1 && (
            <Select.Root value={profile} onValueChange={setProfile} size="2">
              <Select.Trigger />
              <Select.Content>
                {profiles.map((p) => (
                  <Select.Item key={p} value={p}>
                    {p}
                  </Select.Item>
                ))}
              </Select.Content>
            </Select.Root>
          )}
          {running ? (
            <Button
              color="tomato"
              variant="solid"
              disabled={busy !== null}
              onClick={() => act("stop", () => api.stopApp(cfg.name))}
            >
              <StopIcon /> {busy === "stop" ? "Stopping…" : "Stop"}
            </Button>
          ) : (
            <Button
              disabled={busy !== null}
              onClick={() => act("start", () => api.startApp(cfg.name, profile))}
            >
              <PlayIcon /> {busy === "start" ? "Starting…" : "Start"}
            </Button>
          )}
          <Tooltip content="Open in browser">
            <IconButton
              variant="soft"
              disabled={!running}
              onClick={() => api.openApp(cfg.name)}
            >
              <ExternalLinkIcon />
            </IconButton>
          </Tooltip>
          <Tooltip content={running ? "Stop before removing" : "Remove app"}>
            <IconButton
              variant="soft"
              color="gray"
              disabled={running}
              onClick={async () => {
                await api.removeApp(cfg.name);
                onRemoved();
              }}
            >
              <TrashIcon />
            </IconButton>
          </Tooltip>
        </Flex>
      </Flex>

      {error && (
        <Text size="1" color="tomato" className="mono">
          {error}
        </Text>
      )}

      {/* Port plan */}
      {run && run.portPlan.length > 0 && (
        <Flex gap="2" wrap="wrap" align="center">
          <Text size="1" color="gray" weight="medium">
            PORT PLAN
          </Text>
          {run.portPlan.map((p) => (
            <Badge key={p.service} variant="soft" color={p.note ? "amber" : "cyan"}>
              {p.service} → {p.resolved}
              {p.note ? ` (${p.note})` : ""}
            </Badge>
          ))}
        </Flex>
      )}

      <Separator size="4" />

      {/* Service cards */}
      <Flex gap="2" wrap="wrap">
        {activeServices.map((name) => {
          const sc = cfg.services.find((s) => s.name === name);
          const sr = runByName[name];
          const status = sr?.status ?? "stopped";
          return (
            <Card key={name} style={{ minWidth: 240, flex: "1 1 240px" }}>
              <Flex direction="column" gap="1">
                <Flex align="center" justify="between">
                  <Text weight="bold">{name}</Text>
                  <Badge color={STATUS_BADGE[status]} variant="soft">
                    {status}
                  </Badge>
                </Flex>
                <Code size="1" variant="ghost" color="gray">
                  {sr?.resolvedCommand ?? sc?.command ?? ""}
                </Code>
                <Flex gap="3" mt="1">
                  {sr?.port != null && (
                    <Text size="1" color="gray">
                      port <b>{sr.port}</b>
                    </Text>
                  )}
                  {sr?.pid != null && (
                    <Text size="1" color="gray">
                      pid {sr.pid}
                    </Text>
                  )}
                  {sc?.dependsOn && sc.dependsOn.length > 0 && (
                    <Text size="1" color="gray">
                      ↳ {sc.dependsOn.join(", ")}
                    </Text>
                  )}
                  {sr?.exitCode != null && (
                    <Text size="1" color="tomato">
                      exit {sr.exitCode}
                    </Text>
                  )}
                </Flex>
              </Flex>
            </Card>
          );
        })}
      </Flex>

      {/* Logs */}
      <LogPane logs={logs} services={activeServices} />
    </Flex>
  );
}
