import { useState, type ReactNode } from "react";
import {
  Button,
  Checkbox,
  Dialog,
  Flex,
  IconButton,
  Select,
  Text,
  TextField,
} from "@radix-ui/themes";
import { DownloadIcon, PlusIcon, TrashIcon } from "@radix-ui/react-icons";
import { api, pickEnvFile } from "../api";
import type { AppConfig, HealthCheck, ServiceConfig } from "../types";

type HCType = "none" | "http" | "tcp" | "process" | "log";

function hcType(hc?: HealthCheck): HCType {
  return hc ? hc.type : "none";
}

export function ConfigEditor({
  open,
  onOpenChange,
  app,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  app: AppConfig;
  onSaved: () => void;
}) {
  // Mounted fresh per open by the parent, so initialise the draft once.
  const [draft, setDraft] = useState<AppConfig>(() => structuredClone(app));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function patchService(i: number, patch: Partial<ServiceConfig>) {
    setDraft((d) => {
      const services = d.services.map((s, j) =>
        j === i ? { ...s, ...patch } : s,
      );
      return { ...d, services };
    });
  }
  function setEnvEntries(i: number, entries: [string, string][]) {
    const env: Record<string, string> = {};
    for (const [k, v] of entries) if (k.trim()) env[k.trim()] = v; // last wins, drop blanks
    patchService(i, { env });
  }
  async function importDotenv(i: number) {
    const file = await pickEnvFile();
    if (!file) return;
    const parsed = await api.readDotenv(file);
    setDraft((d) => ({
      ...d,
      services: d.services.map((s, j) => {
        if (j !== i) return s;
        const merged = { ...s.env };
        for (const [k, v] of Object.entries(parsed)) {
          // Never let an imported literal clobber a Harbor-managed ${...} placeholder.
          const cur = merged[k];
          if (cur && /\$\{.*\}/.test(cur)) continue;
          merged[k] = v;
        }
        return { ...s, env: merged };
      }),
    }));
  }
  function setHealth(i: number, type: HCType, extra?: Partial<HealthCheck>) {
    let hc: HealthCheck | undefined;
    if (type === "http")
      hc = {
        type: "http",
        path: (extra as any)?.path ?? "/",
        expect: "2xx-3xx",
      };
    else if (type === "tcp") hc = { type: "tcp" };
    else if (type === "process") hc = { type: "process" };
    else if (type === "log")
      hc = { type: "log", pattern: (extra as any)?.pattern ?? "" };
    else hc = undefined;
    patchService(i, { healthCheck: hc });
  }
  function addService() {
    setDraft((d) => ({
      ...d,
      services: [
        ...d.services,
        {
          name: `service-${d.services.length + 1}`,
          cwd: ".",
          command: "",
          env: {},
          dependsOn: [],
        },
      ],
    }));
  }
  function removeService(i: number) {
    setDraft((d) => {
      const removed = d.services[i].name;
      const profiles = Object.fromEntries(
        Object.entries(d.profiles).map(([p, names]) => [
          p,
          names.filter((n) => n !== removed),
        ]),
      );
      return {
        ...d,
        services: d.services.filter((_, j) => j !== i),
        profiles,
      };
    });
  }
  function toggleProfileService(profile: string, svc: string) {
    setDraft((d) => {
      const cur = d.profiles[profile] ?? [];
      const next = cur.includes(svc)
        ? cur.filter((n) => n !== svc)
        : [...cur, svc];
      return { ...d, profiles: { ...d.profiles, [profile]: next } };
    });
  }
  function addProfile() {
    setDraft((d) => {
      let n = 1;
      let name = "profile";
      while (d.profiles[name]) name = `profile-${++n}`;
      return { ...d, profiles: { ...d.profiles, [name]: [] } };
    });
  }
  function renameProfile(oldName: string, newName: string) {
    setDraft((d) => {
      if (!newName || (d.profiles[newName] && newName !== oldName)) return d;
      const entries = Object.entries(d.profiles).map(([p, v]) =>
        p === oldName ? [newName, v] : [p, v],
      );
      return { ...d, profiles: Object.fromEntries(entries) };
    });
  }
  function removeProfile(name: string) {
    setDraft((d) => {
      const { [name]: _, ...rest } = d.profiles;
      return { ...d, profiles: rest };
    });
  }

  async function save() {
    setBusy(true);
    setError(null);
    try {
      await api.updateApp(app.name, draft);
      onSaved();
      onOpenChange(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const otherNames = (self: string) =>
    draft.services.map((s) => s.name).filter((n) => n !== self);

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="820px" className="config-dialog">
        <div className="config-dialog-head">
          <div className="page-eyebrow">Project settings</div>
          <Dialog.Title>Edit {app.name}</Dialog.Title>
          <Dialog.Description size="2" color="gray" mb="3">
            Changes are saved to Harbor's registry — your project's source is
            never touched.
          </Dialog.Description>
        </div>

        <div className="config-scroll">
          {/* Services */}
          <Flex align="center" justify="between" mb="2">
            <Text size="2" weight="bold">
              Services
            </Text>
            <Button size="1" variant="soft" onClick={addService}>
              <PlusIcon /> Add service
            </Button>
          </Flex>

          <Flex direction="column" gap="3">
            {draft.services.map((s, i) => (
              <div className="field config-service-card" key={i}>
                <Flex gap="2" align="center" mb="2">
                  <TextField.Root
                    style={{ flex: 1, fontWeight: 600 }}
                    value={s.name}
                    onChange={(e) => patchService(i, { name: e.target.value })}
                    placeholder="service name"
                  />
                  <IconButton
                    size="1"
                    variant="soft"
                    color="gray"
                    onClick={() => removeService(i)}
                    aria-label={`Remove ${s.name || `service ${i + 1}`}`}
                  >
                    <TrashIcon />
                  </IconButton>
                </Flex>

                <Grid2>
                  <Labeled label="Command">
                    <TextField.Root
                      value={s.command}
                      onChange={(e) =>
                        patchService(i, { command: e.target.value })
                      }
                      placeholder="node server.js"
                    />
                  </Labeled>
                  <Labeled label="Working dir">
                    <TextField.Root
                      value={s.cwd}
                      onChange={(e) => patchService(i, { cwd: e.target.value })}
                      placeholder="."
                    />
                  </Labeled>
                  <Labeled label="Preferred port">
                    <TextField.Root
                      type="number"
                      value={s.port?.toString() ?? ""}
                      onChange={(e) =>
                        patchService(i, {
                          port: e.target.value
                            ? Number(e.target.value)
                            : undefined,
                        })
                      }
                      placeholder="(none)"
                    />
                  </Labeled>
                  <Labeled label="Ready log pattern">
                    <TextField.Root
                      value={s.readyLogPattern ?? ""}
                      onChange={(e) =>
                        patchService(i, {
                          readyLogPattern: e.target.value || undefined,
                        })
                      }
                      placeholder="e.g. listening on"
                    />
                  </Labeled>
                  <Labeled label="Health check">
                    <Select.Root
                      value={hcType(s.healthCheck)}
                      onValueChange={(v) => setHealth(i, v as HCType)}
                    >
                      <Select.Trigger style={{ width: "100%" }} />
                      <Select.Content>
                        <Select.Item value="none">None</Select.Item>
                        <Select.Item value="http">HTTP</Select.Item>
                        <Select.Item value="tcp">TCP</Select.Item>
                        <Select.Item value="process">Process alive</Select.Item>
                        <Select.Item value="log">Log pattern</Select.Item>
                      </Select.Content>
                    </Select.Root>
                  </Labeled>
                  {s.healthCheck?.type === "http" && (
                    <Labeled label="HTTP path">
                      <TextField.Root
                        value={s.healthCheck.path}
                        onChange={(e) =>
                          setHealth(i, "http", { path: e.target.value } as any)
                        }
                        placeholder="/"
                      />
                    </Labeled>
                  )}
                  {s.healthCheck?.type === "log" && (
                    <Labeled label="Health log pattern">
                      <TextField.Root
                        value={s.healthCheck.pattern}
                        onChange={(e) =>
                          setHealth(i, "log", {
                            pattern: e.target.value,
                          } as any)
                        }
                      />
                    </Labeled>
                  )}
                </Grid2>

                <Labeled label="Environment (supports ${PORT}, ${services.X.port})">
                  <Flex direction="column" gap="1">
                    {Object.entries(s.env).map(([k, v], r) => (
                      <Flex gap="2" key={r} align="center">
                        <TextField.Root
                          size="1"
                          style={{
                            flex: "0 0 38%",
                            fontFamily: "var(--font-mono)",
                          }}
                          value={k}
                          placeholder="KEY"
                          onChange={(e) => {
                            const entries = Object.entries(s.env);
                            entries[r] = [e.target.value, v];
                            setEnvEntries(i, entries);
                          }}
                        />
                        <span style={{ color: "var(--text-3)" }}>=</span>
                        <TextField.Root
                          size="1"
                          style={{ flex: 1, fontFamily: "var(--font-mono)" }}
                          value={v}
                          placeholder="value"
                          onChange={(e) => {
                            const entries = Object.entries(s.env);
                            entries[r] = [k, e.target.value];
                            setEnvEntries(i, entries);
                          }}
                        />
                        <IconButton
                          size="1"
                          variant="soft"
                          color="gray"
                          onClick={() =>
                            setEnvEntries(
                              i,
                              Object.entries(s.env).filter((_, j) => j !== r),
                            )
                          }
                        >
                          <TrashIcon />
                        </IconButton>
                      </Flex>
                    ))}
                    <Flex gap="2" mt="1" align="center">
                      <Button
                        size="1"
                        variant="soft"
                        onClick={() =>
                          setEnvEntries(i, [...Object.entries(s.env), ["", ""]])
                        }
                      >
                        <PlusIcon /> Add variable
                      </Button>
                      <Button
                        size="1"
                        variant="soft"
                        color="gray"
                        onClick={() => importDotenv(i)}
                      >
                        <DownloadIcon /> Import .env
                      </Button>
                    </Flex>
                    <Text size="1" color="gray">
                      Stored as plaintext. Env changes apply on next Start.
                    </Text>
                  </Flex>
                </Labeled>

                {otherNames(s.name).length > 0 && (
                  <Labeled label="Depends on">
                    <Flex gap="3" wrap="wrap" pt="1">
                      {otherNames(s.name).map((n) => (
                        <Text as="label" size="1" key={n} className="row">
                          <Checkbox
                            checked={s.dependsOn.includes(n)}
                            onCheckedChange={(c) =>
                              patchService(i, {
                                dependsOn: c
                                  ? [...s.dependsOn, n]
                                  : s.dependsOn.filter((x) => x !== n),
                              })
                            }
                          />
                          {n}
                        </Text>
                      ))}
                    </Flex>
                  </Labeled>
                )}
              </div>
            ))}
          </Flex>

          {/* Profiles */}
          <Flex align="center" justify="between" mt="4" mb="2">
            <Text size="2" weight="bold">
              Profiles
            </Text>
            <Button size="1" variant="soft" onClick={addProfile}>
              <PlusIcon /> Add profile
            </Button>
          </Flex>
          <Flex direction="column" gap="2">
            {Object.entries(draft.profiles).map(([p, names]) => (
              <div className="field config-profile-card" key={p}>
                <Flex gap="2" align="center" mb="2">
                  <TextField.Root
                    size="1"
                    style={{ width: 180 }}
                    defaultValue={p}
                    onBlur={(e) => renameProfile(p, e.target.value.trim())}
                  />
                  <span className="spacer" />
                  <IconButton
                    size="1"
                    variant="soft"
                    color="gray"
                    onClick={() => removeProfile(p)}
                    aria-label={`Remove ${p} profile`}
                  >
                    <TrashIcon />
                  </IconButton>
                </Flex>
                <Flex gap="3" wrap="wrap">
                  {draft.services.map((s) => (
                    <Text as="label" size="1" key={s.name} className="row">
                      <Checkbox
                        checked={names.includes(s.name)}
                        onCheckedChange={() => toggleProfileService(p, s.name)}
                      />
                      {s.name}
                    </Text>
                  ))}
                </Flex>
              </div>
            ))}
          </Flex>
        </div>

        {error && (
          <Text size="1" color="tomato" className="mono" mt="2">
            {error}
          </Text>
        )}

        <Flex className="config-footer" gap="3" mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </Dialog.Close>
          <Button onClick={save} disabled={busy}>
            Save changes
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  );
}

function Grid2({ children }: { children: ReactNode }) {
  return <div className="config-grid">{children}</div>;
}

function Labeled({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="config-labeled">
      <div className="config-label">{label}</div>
      {children}
    </label>
  );
}
