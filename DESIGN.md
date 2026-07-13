# Harbor — shipped architecture

> Harbor is a trusted local runtime control plane for people and coding agents:
> one place to understand what is running, prevent accidental duplicates, and
> safely operate a project's local services.

This document describes the architecture shipped in **Harbor v0.4.1**. Product
priorities that are not implemented yet belong in [`ROADMAP.md`](./ROADMAP.md),
and the signed release procedure belongs in
[`DISTRIBUTING.md`](./DISTRIBUTING.md).

Stack: **Tauri 2** with a Rust core and a React 19 / Radix Themes web UI. The
current production target is **macOS 11 or newer** on Apple Silicon and Intel.

---

## 1. Product boundary and invariants

Harbor manages registered project services, but it also inventories local TCP
listeners started by terminals, IDEs, and coding agents. Those are deliberately
different capabilities. The implementation follows these rules:

- **Observation is not ownership.** Matching a listener to a project does not
  automatically grant permission to terminate its process.
- **Agent-written commands require local approval.** A config registered over
  MCP cannot execute until a person reviews and approves that exact config in
  Harbor.
- **A matching server is reused before a duplicate is launched.** Ambiguous or
  unsafe matches block launch instead of making Harbor guess.
- **Destructive actions require fresh identity evidence.** Listener cleanup
  revalidates its process-group leader, start-time token, and listening port;
  adopted-process signals revalidate the recorded process identity.
- **Remote control is loopback-only.** MCP binds to `127.0.0.1` and requires a
  bearer token that changes on every Harbor launch.
- **The desktop app and managed services have separate lifetimes.** Quitting or
  updating Harbor does not intentionally stop its project processes; a later
  Harbor launch verifies and re-adopts survivors.

## 2. Core concepts

- **App / project** — a registered project folder with a name, absolute root,
  services, profiles, and local approval state. Rust uses `AppConfig`; the UI
  generally calls these projects.
- **Service** — one long-running command with a working directory, optional
  preferred port, environment values, dependencies, and readiness policy.
- **Profile** — a named subset of an app's services, such as `default` or `dev`.
- **Run** — the live in-memory state for an app: service status, process-group
  leader, resolved ports and commands, recent logs, and resource samples.
- **Port plan** — each service's preferred and resolved port, including notes
  about bumps, fixed ports, rewiring, or reuse of an existing server.
- **Local server** — a current-user TCP listener observed through macOS process
  inspection. It can be unknown, matched, Harbor-managed, or safely cleanable.
- **Adopted service** — a verified process represented in Harbor without a live
  `Child` handle. It may be a Harbor-spawned survivor from an earlier session or
  a strongly corroborated server that was started outside Harbor.

## 3. Architecture

```text
┌──────────────────────────────── Harbor.app ────────────────────────────────┐
│                                                                            │
│  React webview                                                             │
│  projects · service cards · logs · local servers · connections · updates   │
│             │ Tauri IPC                     ▲ Tauri events                 │
│             ▼                               │                              │
│  Tauri commands ───────▶ shared operations ─┴──────┐                       │
│                                                    ▼                       │
│                                        Arc<AppState>                       │
│                              registry · supervisor · MCP settings           │
│                                 │          │          │                    │
│                   flat JSON ◀───┘          │          └──▶ axum + rmcp     │
│                                            │               /mcp            │
│                                  process groups, logs,       ▲              │
│                                  health, resources           │ bearer token │
│                                            │                 │              │
│                   listener discovery ◀──── macOS ───── AI client/bridge    │
└────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      └── signed updater ──▶ GitHub Releases
```

`AppState` is the live boundary shared by the Tauri command layer and the MCP
server. Both surfaces see the same registry and supervisor state. Common
lifecycle behavior lives in `ops.rs`; MCP applies a stricter stop policy where
remote observation must not expand into authority over externally started
processes.

### 3.1 Rust core

- **Supervisor:** starts services with `sh -c` in a new Unix session/process
  group (`setsid`), captures stdout and stderr, tracks status, retains bounded
  recent logs, samples process-group CPU and RSS, and stops the whole verified
  group with SIGTERM followed by SIGKILL when required.
- **Readiness:** a service becomes ready through an HTTP 2xx/3xx probe, TCP
  connection, log match, or process-alive policy. Dependencies wait for their
  prerequisites before starting.
- **State and validation:** config writes pass structural validation, and
  lifecycle-sensitive mutations use per-app locks. Registry persistence
  completes before the in-memory version changes.
- **Discovery:** `lsof` and `ps` provide current-user listener, process-group,
  command, start-time, and working-directory evidence. Bounded HTTP probes add
  response status, page title, and server-header hints.
- **Events:** the supervisor emits `harbor://log`, `harbor://status`,
  `harbor://registry`, and `harbor://stats` events to keep the UI synchronized.
- **Recovery:** persisted run identities allow a relaunched Harbor to verify and
  re-adopt processes it previously spawned. Opt-in auto-restart uses bounded
  backoff only for Harbor-spawned services that fail unexpectedly.

### 3.2 MCP server

The MCP server runs in-process on Tauri's Tokio runtime and shares
`Arc<AppState>` with the supervisor. It uses `rmcp` Streamable HTTP nested at
`/mcp` in an `axum` router. Requests are stateless and return JSON responses;
Harbor does not issue an MCP session ID or expose a standalone SSE stream.

- Harbor reserves the loopback socket during startup, preferring port `7777`
  and scanning upward if necessary, before publishing connection details.
- A cryptographically random bearer token is generated on every launch. The
  live port and token are written to the protected `mcp.json` descriptor.
- Harbor is single-instance. A second launch focuses the existing window rather
  than racing the endpoint or descriptor.
- One-click Claude Code, Claude Desktop, and Codex setup installs an owner-only
  launcher that reads the live descriptor, opens Harbor quietly when necessary,
  and runs the pinned `mcp-remote@0.1.38` adapter.
- The restart-safe launcher currently requires Node.js/npx and may need network
  access on first use. Advanced users can connect directly with native HTTP,
  but that configuration is tied to the current launch's port and token.
- Every tool advertises object-form input and output schemas. In particular,
  the uniform `result` output is an object schema rather than JSON Schema's
  boolean `true` form, which some desktop MCP hosts reject even though it is
  valid JSON Schema.
- The AI connections UI reports managed launcher configuration separately from
  an observed **Bridge running** process. Process observation cannot prove a
  host accepted the tool catalog, so client compatibility is verified with a
  real host restart plus `scripts/mcp-bridge-soak.mjs`.

### 3.3 Web UI and menu bar

The React UI uses Tauri commands for actions and Tauri events for live state. Its
main surfaces are project overview and service detail, local-server inventory,
AI-agent connections, project settings, and app updates. A native menu-bar
popover exposes common start, stop, and open actions without opening the main
window. Window size and position are restored between launches; a duplicate app
launch brings the existing main window forward.

## 4. Port intelligence

1. A service can declare a preferred `port` and consume it with `${PORT}` in
   its command or environment.
2. Cross-service references use `${services.<name>.port}`, allowing a frontend
   to target the backend's resolved port.
3. Before allocation, Harbor searches the project root for already-running
   listeners and attempts to corroborate them to configured services. One strong
   match is reused; multiple candidates or a project-related unsafe match stop
   the launch and direct the user to Local servers.
4. Relocatable services try the preferred port, then scan upward for a free one.
   A port is considered free only if both IPv4 and IPv6 wildcard binds succeed.
5. Commands with a literal port flag, or services that do not consume a port
   placeholder, are treated as fixed. Harbor never reports a bumped port that
   the command cannot actually use.
6. Ports already reserved by another live Harbor run are excluded. Existing
   external claims remain in the same port map so dependent placeholders point
   at the reused service.
7. Services are topologically sorted by `dependsOn`. Commands and environment
   values are resolved from the completed port map before each process starts.

The resulting port plan is returned to the UI and MCP clients so they can
explain what Harbor reused, allocated, or rewired.

## 5. Config schema

The shareable project format is `harbor.json`:

```jsonc
{
  "name": "Example",
  "root": "/Users/me/code/example",
  "services": [
    {
      "name": "api",
      "cwd": ".",
      "command": "npm run api -- --port ${PORT}",
      "port": 4321,
      "env": { "PORT": "${PORT}" },
      "dependsOn": [],
      "healthCheck": {
        "type": "http",
        "path": "/health",
        "expect": "2xx-3xx"
      }
    },
    {
      "name": "web",
      "cwd": ".",
      "command": "npm run dev -- --port ${PORT}",
      "port": 5173,
      "env": {
        "API_URL": "http://127.0.0.1:${services.api.port}"
      },
      "dependsOn": ["api"],
      "readyLogPattern": "Local:"
    }
  ],
  "profiles": {
    "default": ["api", "web"],
    "api-only": ["api"]
  },
  "autoRestart": false
}
```

Supported readiness types are `http`, `tcp`, `log`, and `process`.
`readyLogPattern` is also supported as a direct log-readiness field. If no
explicit `default` profile exists, `default` selects all services.

Harbor validates absolute roots, existing working directories, unique service
names, dependency references and cycles, profile contents, ports, environment
keys, and readiness requirements before a config reaches the executable path.
The internal `trusted` flag is machine-local approval state and is deliberately
removed from exported `harbor.json` files.

## 6. MCP tool surface and approval flow

The v0.4.1 MCP server exposes these tools:

| Tool | Purpose |
|------|---------|
| `list_apps` | List registered apps, profiles, services, and live run state. |
| `app_status(app)` | Return per-service status, resolved ports, and port plan. |
| `detect_app(path)` | Inspect a project folder and propose a config without saving it. |
| `register_app(config)` | Add or replace a config as approval-required; never starts it. |
| `start_app(app, profile?)` | Start an approved app, reusing strong external matches first. |
| `stop_app(app)` | Stop Harbor-managed processes only. |
| `restart_app(app, profile?)` | Restart a managed app under the same or selected profile. |
| `get_logs(app, service, lines?)` | Return recent captured log lines, capped at 2,000. |
| `list_local_servers` | Inventory local listeners, matches, duplicates, and cleanup safety. |
| `stop_local_server(pid, port, startedAt)` | Stop one identity-verified untracked server previously marked safe. |
| `open_app(app)` | Open the running app's primary local URL. |

Inputs have generated JSON schemas, outputs use a consistent object-rooted
shape, and every tool advertises read-only, destructive, idempotence, and
open-world hints.

Approval is config-scoped, not a prompt for every lifecycle call:

1. `detect_app` only observes and proposes.
2. `register_app` always forces an MCP-supplied config to untrusted, even if its
   payload claims otherwise.
3. `start_app` refuses to execute an untrusted config.
4. Harbor's local UI shows the config and approves only if it is structurally
   unchanged from what the person reviewed. A concurrent agent edit invalidates
   the approval attempt.
5. After approval, lifecycle tools can operate the config. A later MCP
   replacement requires approval again.

Configs created through an explicit local UI action are locally trusted. MCP
cannot call the approval command. For stopping, MCP uses the managed-only policy;
an externally started process must be reviewed and confirmed in Harbor's UI or
cleaned through `stop_local_server` when the inventory explicitly reports
`safeToStop: true`.

## 7. Security model

- MCP is reachable only on loopback and requires a per-launch bearer token.
- The app-data directory is owner-only (`0700`). Central registry, run,
  credential, agent-config, and safety-backup files are written owner-only
  (`0600`) on Unix; the executable MCP launcher is `0700`.
- Private central-state and agent-config writes are atomic, reducing
  partial-file corruption. Registry persistence completes before live state is
  replaced.
- Harbor runs arbitrary local commands by design and therefore is not an
  App-Sandbox application. Commands proposed by agents remain blocked behind
  the local trust gate.
- Local-listener matching is evidence, not authority. Unsafe process groups
  hosted by shells, terminals, IDEs, Claude, Codex, or Harbor remain
  monitor-only and can still block an accidental duplicate launch.
- Network-visible binds are labeled as warnings; Harbor does not interpret that
  label as a firewall or exposure guarantee.
- Service environment values are currently stored in the protected JSON
  registry. v0.4.1 does not yet move secrets into Keychain; that work is tracked
  in [`ROADMAP.md`](./ROADMAP.md).

## 8. Project auto-detection

`detect_app` is a read-only heuristic scanner used by drag-and-drop onboarding
and MCP. It recognizes package scripts and common JavaScript frameworks, honors
pnpm/yarn/bun lockfiles, and can propose services for Django, FastAPI, Flask,
Go, Rails, Procfiles, and static sites. Compose files and Makefiles are reported
as notes but are not automatically converted into runnable services in v0.4.1.

Detection returns a proposed config plus human-readable evidence. It never
saves, trusts, or starts that proposal. The user or agent may correct it before
registration; the registration surface then determines whether local approval
is required.

## 9. Local-server inventory and matching

The inventory is a fresh snapshot of **TCP listeners owned by the current macOS
user**, not every process or every network service on the machine.

For each listener Harbor records the socket PID and addresses, walks to the
process-group leader, obtains command/start-time/cwd evidence, infers a nearby
project root, classifies likely frameworks or infrastructure, and probes likely
development HTTP endpoints for status, title, and server header. It then:

- maps a listener to the longest matching registered project root;
- selects a service when configured port or command evidence is strong enough;
- distinguishes tracked, external, unmatched, and Harbor-internal listeners;
- flags wildcard or LAN-facing binds as network-visible;
- groups probable duplicate project/runtime fingerprints across distinct
  process groups; and
- keeps non-development listeners available behind the inventory filter rather
  than hiding them permanently.

Inventory matching is intentionally broader than adoption. A project-level
match may help a person understand a listener while remaining insufficient for
Harbor to claim the process as a service.

## 10. Adoption, cleanup, and process lifetime

### Harbor-spawned survivors

Every spawned service is a process-group leader. Harbor stores its app/service,
PID, port, resolved command, cwd, profile, and exact `ps` start-time token in
`runs.json`. On the next launch, a record is adopted only when the PID still
exists as the group leader, the start time and command match, and any recorded
port is still owned by that group. Stale records are pruned without signalling.

Adopted survivors retain status, port, open, stop, and resource monitoring, but
stdout/stderr cannot be reattached after the original Harbor process exits.

### Externally started servers

Before allocating, Harbor can adopt one externally started service when its
project root, configured service, command, port, and isolated process group
corroborate. The observed identity is persisted and revalidated like other
adopted services. If a listener belongs to the project but the evidence is
ambiguous or its group is hosted by a terminal, IDE, or agent, Harbor refuses to
take control and refuses to launch a duplicate.

The locally confirmed UI may stop a corroborated external service after making
the process-group impact visible. MCP `stop_app` cannot: remote callers must not
gain termination authority merely because Harbor observed a match.

### Inventory cleanup

`safeToStop` is true only for an untracked, non-Harbor listener in an isolated
process group with an allowed development-runtime classification. Cleanup
requires the exact group-leader PID, port, and `startedAt` token from a recent
inventory result. Harbor takes a fresh snapshot immediately before SIGTERM and
again before any SIGKILL escalation. A changed identity, unsafe group host,
Harbor-managed process, or surviving unrelated group is refused.

### Crash recovery

Unexpected failures generate native notifications. If `autoRestart` is enabled,
Harbor applies bounded backoff and a give-up cap to Harbor-spawned services only.
Intentional stops, clean exit after readiness, untrusted config changes, app
shutdown, and adopted/external services do not trigger automatic restarts.

## 11. Distribution and signed updates

Production releases are universal macOS builds distributed as a DMG through
GitHub Releases and the Homebrew tap. The protected tag workflow:

1. verifies the release tag and all version fields against `main`;
2. runs frontend and Rust tests, formatting, linting, and bridge syntax checks;
3. builds for Apple Silicon and Intel;
4. signs with **Developer ID Application: Faba Development LLC**;
5. notarizes and staples both the app and finished DMG;
6. signs the updater archive with Harbor's separate Tauri updater key;
7. creates a draft release and verifies the updater signature, Apple signature,
   Gatekeeper acceptance, and notarization before publication.

The release contains a universal DMG, universal app updater archive and
signature, and `latest.json` entries for both Mac architectures. This is the
supported distribution path; users should not need unsigned-app workarounds.

Production builds check the public `latest.json` feed shortly after launch and
every six hours. A manual check is available in Settings. Harbor never installs
silently: it shows the new version and notes, waits for **Update and restart**,
verifies the archive against the embedded updater public key, installs it, and
relaunches. Choosing **Later** suppresses that version's automatic prompt for 24
hours without hiding it from manual checks. Managed project services remain
online through the desktop-app restart and are re-adopted afterward.

`npm run tauri:build:local` creates local app/DMG bundles without updater
artifacts; it is not the signed release workflow. Because it is still a
production frontend build, the packaged app can check the configured public
feed. `npm run tauri dev` does not perform automatic update checks.

## 12. Persistence layout

Harbor uses flat JSON, not SQLite. The central source of truth lives under:

```text
~/Library/Application Support/com.harbor.desktop/
```

| Path | Contents |
|------|----------|
| `registry.json` | Map of app name to full `AppConfig`, including local trust state. |
| `runs.json` | Process identities used for verified adoption after relaunch. |
| `mcp.json` | Current MCP port and per-launch bearer token. |
| `harbor-mcp-bridge` | Restart-safe owner-only launcher installed by agent setup. |

The in-memory registry is an `RwLock<BTreeMap<String, AppConfig>>` shared with
the supervisor. Mutations clone and validate the next registry, atomically save
it, then replace live state and notify the supervisor. `runs.json` updates are
serialized separately because monitor, stop, and adoption tasks can all change
run identities.

A project-level `harbor.json` can be imported or exported for sharing. The
central registry remains authoritative, an omitted import root defaults to the
file's project directory, and exported configs never carry machine-local trust.

## 13. Module map and verification

| Module | Responsibility |
|--------|----------------|
| `model.rs` | Persisted config types, run snapshots, events, and listener inventory types. |
| `store.rs` | Flat-JSON registry, MCP descriptor, run identities, and import/export. |
| `state.rs` | Shared registry/state, validation, atomic mutation, and approval preconditions. |
| `supervisor.rs` | Process groups, logs, readiness, resources, adoption, stopping, and restart. |
| `ports.rs` | Dependency ordering, dual-stack allocation, pin detection, and placeholders. |
| `health.rs` | HTTP and TCP readiness probes. |
| `detect.rs` | Project and framework detection heuristics. |
| `discovery.rs` | Listener inventory, project matching, duplicate grouping, and safe cleanup. |
| `ops.rs` | Shared lifecycle and open operations. |
| `commands.rs` | Tauri IPC commands, agent setup, import/export, and AI-fix integration. |
| `mcp.rs` | Authenticated `rmcp` Streamable-HTTP server and tool definitions. |
| `tray.rs` | Menu-bar icon and popover behavior. |
| `lib.rs` | Tauri plugins, startup wiring, adoption, MCP hosting, and shutdown behavior. |

Primary local verification:

```bash
npm test
npm run build

(
  cd src-tauri
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets -- -D warnings
  cargo test --locked
)

git diff --check
```

For MCP-facing changes, run the patched app with an existing one-click client
configuration and add this live bridge check:

```bash
node scripts/mcp-bridge-soak.mjs --duration-ms 90000 --interval-ms 30000
```
