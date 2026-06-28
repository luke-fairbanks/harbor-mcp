import { useState } from "react";
import { Badge, Button, Code, Dialog, Flex, Text } from "@radix-ui/themes";
import { DownloadIcon, FileIcon } from "@radix-ui/react-icons";
import { api, pickFolder } from "../api";
import type { Detection } from "../types";

export function RegisterDialog({
  open,
  onOpenChange,
  onRegistered,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onRegistered: (name: string) => void;
}) {
  const [path, setPath] = useState<string | null>(null);
  const [detection, setDetection] = useState<Detection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function reset() {
    setPath(null);
    setDetection(null);
    setError(null);
  }

  async function choose() {
    const dir = await pickFolder("Choose a project folder");
    if (!dir) return;
    setPath(dir);
    setDetection(null);
    setError(null);
    setBusy(true);
    try {
      setDetection(await api.detectApp(dir));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function register() {
    if (!detection) return;
    setBusy(true);
    try {
      await api.registerApp(detection.proposed);
      onRegistered(detection.proposed.name);
      onOpenChange(false);
      reset();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function importJson() {
    if (!path) return;
    setBusy(true);
    setError(null);
    try {
      const cfg = await api.importApp(path);
      onRegistered(cfg.name);
      onOpenChange(false);
      reset();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(v) => {
        onOpenChange(v);
        if (!v) reset();
      }}
    >
      <Dialog.Content maxWidth="560px">
        <Dialog.Title>Register an app</Dialog.Title>
        <Dialog.Description size="2" color="gray" mb="3">
          Choose a project folder. Harbor scans it and proposes a config — nothing
          runs until you start it.
        </Dialog.Description>

        <Flex gap="2" align="center" mb="3">
          <Button onClick={choose} disabled={busy}>
            <FileIcon /> Choose folder…
          </Button>
          {path && (
            <Code variant="ghost" className="mono" style={{ fontSize: 11 }}>
              {path}
            </Code>
          )}
        </Flex>

        {error && (
          <Text size="1" color="tomato" className="mono" as="p" mb="2">
            {error}
          </Text>
        )}

        {detection && (
          <div className="field" style={{ margin: "0 0 8px" }}>
            <Flex align="center" gap="2" mb="2">
              <Text weight="bold">{detection.proposed.name}</Text>
              <Badge variant="soft">
                {detection.proposed.services.length} service
                {detection.proposed.services.length === 1 ? "" : "s"}
              </Badge>
            </Flex>
            <Flex direction="column" gap="2">
              {detection.proposed.services.map((s) => (
                <Flex key={s.name} gap="2" align="baseline">
                  <span className="chip" data-tone="accent">
                    {s.name}
                    {s.port ? `:${s.port}` : ""}
                  </span>
                  <Code size="1" variant="ghost" color="gray">
                    {s.command}
                  </Code>
                </Flex>
              ))}
            </Flex>
            <Text size="1" color="gray" mt="2" as="div">
              {detection.notes.map((n, i) => (
                <div key={i}>· {n}</div>
              ))}
            </Text>
          </div>
        )}

        <Flex gap="3" mt="4" justify="between" align="center">
          {path ? (
            <Button variant="ghost" size="1" onClick={importJson} disabled={busy}>
              <DownloadIcon /> Import existing harbor.json
            </Button>
          ) : (
            <span />
          )}
          <Flex gap="3">
            <Dialog.Close>
              <Button variant="soft" color="gray">
                Cancel
              </Button>
            </Dialog.Close>
            <Button
              onClick={register}
              disabled={
                !detection || detection.proposed.services.length === 0 || busy
              }
            >
              Register
            </Button>
          </Flex>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  );
}
