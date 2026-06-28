import { useState } from "react";
import {
  Badge,
  Button,
  Code,
  Dialog,
  Flex,
  Text,
  TextField,
} from "@radix-ui/themes";
import { MagnifyingGlassIcon } from "@radix-ui/react-icons";
import { api } from "../api";
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
  const [path, setPath] = useState("");
  const [detection, setDetection] = useState<Detection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function reset() {
    setPath("");
    setDetection(null);
    setError(null);
  }

  async function scan() {
    setBusy(true);
    setError(null);
    setDetection(null);
    try {
      setDetection(await api.detectApp(path.trim()));
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

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(v) => {
        onOpenChange(v);
        if (!v) reset();
      }}
    >
      <Dialog.Content maxWidth="620px">
        <Dialog.Title>Register an app</Dialog.Title>
        <Dialog.Description size="2" color="gray" mb="3">
          Point Harbor at a project folder. It scans for services and proposes a
          config — nothing is run until you start it.
        </Dialog.Description>

        <Flex gap="2" mb="3">
          <TextField.Root
            placeholder="/Users/you/Desktop/my-project"
            value={path}
            style={{ flex: 1 }}
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && path.trim() && scan()}
          />
          <Button onClick={scan} disabled={!path.trim() || busy}>
            <MagnifyingGlassIcon /> Scan
          </Button>
        </Flex>

        {error && (
          <Text size="1" color="tomato" className="mono">
            {error}
          </Text>
        )}

        {detection && (
          <Flex direction="column" gap="2">
            <Flex align="center" gap="2">
              <Text weight="bold">{detection.proposed.name}</Text>
              <Badge variant="soft">
                {detection.proposed.services.length} service
                {detection.proposed.services.length === 1 ? "" : "s"}
              </Badge>
            </Flex>
            {detection.proposed.services.map((s) => (
              <Flex key={s.name} gap="2" align="baseline">
                <Badge color="cyan" variant="soft">
                  {s.name}
                  {s.port ? `:${s.port}` : ""}
                </Badge>
                <Code size="1" variant="ghost" color="gray">
                  {s.command}
                </Code>
              </Flex>
            ))}
            <Text size="1" color="gray" mt="1">
              {detection.notes.map((n, i) => (
                <div key={i}>• {n}</div>
              ))}
            </Text>
          </Flex>
        )}

        <Flex gap="3" mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </Dialog.Close>
          <Button
            onClick={register}
            disabled={!detection || detection.proposed.services.length === 0 || busy}
          >
            Register
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  );
}
