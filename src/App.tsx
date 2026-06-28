import { useCallback, useEffect, useState } from "react";
import {
  Box,
  Button,
  Flex,
  Heading,
  IconButton,
  Text,
  Tooltip,
} from "@radix-ui/themes";
import { GearIcon, PlusIcon } from "@radix-ui/react-icons";
import { api, onLog, onStatus } from "./api";
import type { AppListItem, AppRunSnapshot, LogLine } from "./types";
import { StatusDot, aggregateStatus } from "./components/StatusDot";
import { AppDetail } from "./components/AppDetail";
import { SettingsPanel } from "./components/SettingsPanel";
import { RegisterDialog } from "./components/RegisterDialog";

const LOG_CAP = 4000;

export default function App() {
  const [items, setItems] = useState<AppListItem[]>([]);
  const [live, setLive] = useState<Record<string, AppRunSnapshot>>({});
  const [logs, setLogs] = useState<Record<string, LogLine[]>>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<"app" | "settings">("app");
  const [registerOpen, setRegisterOpen] = useState(false);

  const refreshList = useCallback(async () => {
    const list = await api.listApps();
    setItems(list);
    setLive((prev) => {
      const next = { ...prev };
      for (const it of list) if (it.run) next[it.config.name] = it.run;
      return next;
    });
    setSelected((cur) => cur ?? list[0]?.config.name ?? null);
  }, []);

  const refreshApp = useCallback(async (app: string) => {
    const snap = await api.appStatus(app);
    setLive((prev) => ({
      ...prev,
      [app]: snap ?? { app, running: false, services: [], portPlan: [] },
    }));
  }, []);

  useEffect(() => {
    refreshList();
  }, [refreshList]);

  // Live log + status streams from the supervisor.
  useEffect(() => {
    let cancelled = false;
    let offLog: (() => void) | undefined;
    let offStatus: (() => void) | undefined;

    onLog((l) => {
      setLogs((prev) => {
        const arr = prev[l.app] ? prev[l.app].concat(l) : [l];
        if (arr.length > LOG_CAP) arr.splice(0, arr.length - LOG_CAP);
        return { ...prev, [l.app]: arr };
      });
    }).then((u) => (cancelled ? u() : (offLog = u)));

    onStatus((s) => {
      refreshApp(s.app);
    }).then((u) => (cancelled ? u() : (offStatus = u)));

    return () => {
      cancelled = true;
      offLog?.();
      offStatus?.();
    };
  }, [refreshApp]);

  const selectedItem = items.find((i) => i.config.name === selected) ?? null;

  return (
    <Box className="harbor-shell">
      {/* Sidebar */}
      <Box className="harbor-sidebar">
        <Flex align="center" justify="between" px="3" py="3">
          <Heading size="4" style={{ letterSpacing: "-0.02em" }}>
            ⚓ Harbor
          </Heading>
          <Tooltip content="Register an app">
            <IconButton
              variant="soft"
              size="1"
              onClick={() => setRegisterOpen(true)}
            >
              <PlusIcon />
            </IconButton>
          </Tooltip>
        </Flex>

        <Box className="harbor-applist">
          {items.length === 0 && (
            <Text size="1" color="gray" as="p" style={{ padding: "8px 16px" }}>
              No apps yet. Click + to register one.
            </Text>
          )}
          {items.map((it) => {
            const run = live[it.config.name];
            const status = aggregateStatus(run);
            return (
              <div
                key={it.config.name}
                className="harbor-app-item"
                data-selected={view === "app" && selected === it.config.name}
                onClick={() => {
                  setSelected(it.config.name);
                  setView("app");
                }}
              >
                <StatusDot status={status} />
                <Box style={{ minWidth: 0, flex: 1 }}>
                  <Text size="2" weight="medium" truncate as="div">
                    {it.config.name}
                  </Text>
                </Box>
                {status !== "stopped" && (
                  <Text size="1" color="gray">
                    {status}
                  </Text>
                )}
              </div>
            );
          })}
        </Box>

        <Box px="2" py="2">
          <Button
            variant={view === "settings" ? "soft" : "ghost"}
            color="gray"
            style={{ width: "100%", justifyContent: "flex-start" }}
            onClick={() => setView("settings")}
          >
            <GearIcon /> Connect your Claude
          </Button>
        </Box>
      </Box>

      {/* Detail */}
      <Box className="harbor-detail">
        {view === "settings" ? (
          <SettingsPanel />
        ) : selectedItem ? (
          <AppDetail
            key={selectedItem.config.name}
            item={selectedItem}
            run={live[selectedItem.config.name]}
            logs={logs[selectedItem.config.name] ?? []}
            onChanged={() => {
              refreshApp(selectedItem.config.name);
              refreshList();
            }}
            onRemoved={() => {
              setSelected(null);
              refreshList();
            }}
          />
        ) : (
          <Flex align="center" justify="center" className="fill">
            <Text color="gray">Select an app, or click + to register one.</Text>
          </Flex>
        )}
      </Box>

      <RegisterDialog
        open={registerOpen}
        onOpenChange={setRegisterOpen}
        onRegistered={(name) => {
          setSelected(name);
          setView("app");
          refreshList();
        }}
      />
    </Box>
  );
}
