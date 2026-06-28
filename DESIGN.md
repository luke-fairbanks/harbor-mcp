# Harbor — design & build plan

> A native desktop app that boots all the local servers an app needs with one
> button, manages ports intelligently, and exposes an **MCP server** so any
> user's Claude can discover, configure, and drive it — without editing source.

Status: **design draft** (written in the QuizletLocal session). Build happens in
a fresh session. Stack decision: **Tauri 2** (Rust core + web UI).

---

## 1. Why this exists

Local process managers already exist (foreman, Procfile runners, pm2, Tilt,
Laravel Herd, Tauri's own dev tooling). **None are MCP-native.** Harbor's
differentiator: a person installs it, registers it with their Claude once, and
from then on their Claude can:

- detect what services a project needs (frontend / backend / db / worker…),
- write/adjust the run config (without touching the project's source),
- start/stop everything with correct ordering and **automatic port allocation**,
- read logs and status to debug.

The orchestrator is table stakes; the **Claude-drivable MCP surface** is the product.

## 2. Core concepts

- **App** — a project folder the user has registered. Has a name, root path, and
  a list of services.
- **Service** — one long-running process: `{ name, cwd, command, port?, env,
  dependsOn[], healthCheck, readyLogPattern? }`. Examples: `web` (Vite),
  `api` (Node), `db` (postgres/docker).
- **Run** — a live instance of an app: each service is a spawned child process
  with resolved ports, captured logs, and a status (`stopped|starting|ready|
  unhealthy|exited`).
- **Port plan** — the concrete port each service got this run (preferred port, or
  the next free one if taken), plus how dependents were rewired to it.

## 3. Architecture (Tauri 2)

```
┌─────────────────────────────────────────────────────────────┐
│  Harbor.app (Tauri)                                          │
│                                                              │
│  ┌────────────────────────┐     ┌─────────────────────────┐ │
│  │  Web UI (React)        │◀───▶│  Rust core (tokio)      │ │
│  │  - app list            │ IPC │  - process supervisor   │ │
│  │  - service cards       │ +   │  - port allocator       │ │
│  │  - live logs           │ evt │  - config store (sqlite)│ │
│  │  - config editor       │     │  - health monitor       │ │
│  └────────────────────────┘     └───────────┬─────────────┘ │
│                                              │ shared state  │
│                                  ┌───────────▼─────────────┐ │
│                                  │  MCP server (axum, HTTP)│ │
│                                  │  127.0.0.1:<port> +token│ │
│                                  └───────────┬─────────────┘ │
└──────────────────────────────────────────────┼──────────────┘
                                                │ Streamable HTTP
                                       ┌────────▼─────────┐
                                       │  User's Claude   │
                                       │ (Code / Desktop) │
                                       └──────────────────┘
```

### 3.1 Rust core
- **Process supervisor:** spawn via `tokio::process::Command`. Spawn each service
  in its own **process group** (`setsid` on unix / job object on Windows) so the
  whole child tree can be killed cleanly. Stream stdout/stderr line-by-line and
  emit them as Tauri events (`log://{app}/{service}`) to the UI and into a ring
  buffer (for `get_logs`). Track PIDs; on stop, kill the group; SIGTERM then
  SIGKILL after a grace period.
- **Health monitor:** per service, one of: TCP-connect to its port, HTTP GET a
  path expecting 2xx/3xx, a `readyLogPattern` regex on stdout, or just
  "process alive". Drives the `starting → ready` transition and ordering gates.
- **Config store:** SQLite (via `rusqlite`/`sqlx`) in the app data dir for the
  central registry; optionally read/write a per-project `harbor.json` (see §5)
  so configs are shareable/committable.
- **Crates (verify latest at build):** `tokio`, `axum`, `serde`, `rusqlite` or
  `sqlx`, `sysinfo` (process/port introspection), `command-group` or manual
  process-group handling, `regex`, `which` (resolve binaries incl. nvm/asdf), `rmcp`.

### 3.2 The MCP server (the crux)
- **Hosted in-process** by the running app (an `axum` server on a tokio task),
  sharing `Arc<AppState>` with the supervisor so tools act on **live** state.
  (A stdio MCP server spawned by Claude would be a separate process that can't
  see the GUI's running state — wrong model. Host it.)
- **Transport:** MCP **Streamable HTTP** (current spec). Use the **official Rust
  MCP SDK (`rmcp`)** rather than hand-rolling the protocol; confirm crate name/
  version + that it supports the streamable-HTTP server transport at build time
  (fall back to SSE if needed).
- **Bind:** `127.0.0.1` only, on a stable port (configurable; default e.g. 7777,
  auto-bump if taken). Require a per-install **bearer token**.
- **Registration UX:** Settings screen shows a one-click "Connect your Claude"
  that copies/writes the exact `claude mcp add --transport http harbor
  http://127.0.0.1:<port> --header "Authorization: Bearer <token>"` command (and
  the JSON snippet for Claude Desktop). This is the "others download it and their
  Claude does the same" path.

### 3.3 Web UI
- React (reuse the Radix Themes + framer-motion stack from QuizletLocal for
  consistency and speed). Tauri commands for actions, Tauri events for live logs/
  status. Keep it a single window with a sidebar (apps) + detail (services/logs)
  + a settings page.

## 4. Port intelligence (headline feature)

1. Services declare a **preferred port** and use a placeholder in command/env:
   `command: "vite --port ${PORT}"`, `env: { PORT: "${PORT}" }`.
2. Cross-service references: a service can point at another's resolved port,
   e.g. the web service's proxy target: `${services.api.port}` (or a `links` map).
3. **Allocation on start** (after topological sort by `dependsOn`):
   - For each service, try its preferred port; if `TcpListener::bind` fails
     (taken), scan upward in a range for the first free port.
   - Record the resolved port in the run's **port plan**.
   - Resolve all `${...}` placeholders (its own `${PORT}` + any
     `${services.X.port}`) before spawning.
4. **Dependent rewiring:** because references resolve at launch, the frontend
   automatically targets whatever port the backend actually got. Surface the plan
   in the UI ("api → 4322 (4321 was busy); web proxy updated").
5. Free ports on stop; never reuse a port still held by another Harbor run.

## 5. Config schema (`harbor.json`, per project — also stored centrally)

```jsonc
{
  "name": "QuizletLocal",
  "services": [
    {
      "name": "server",
      "cwd": ".",
      "command": "node server.js",
      "port": 4321,
      "env": { "PORT": "${PORT}" },
      "healthCheck": { "type": "http", "path": "/", "expect": "2xx-3xx" },
      "readyLogPattern": "running →"
    }
    // dev profile could add a "web" (vite) service that dependsOn "server"
    // and proxies ${services.server.port}
  ],
  "profiles": {
    "default": ["server"],
    "dev": ["server", "web"]
  }
}
```

- **Profiles** let one app expose "use it" vs "develop it" service sets — exactly
  the QuizletLocal prod (1 server) vs dev (2 servers) distinction.

## 6. MCP tool surface (initial)

| Tool | Purpose |
|------|---------|
| `list_apps` | Registered apps + current run status |
| `app_status(app)` | Per-service state, resolved ports, health |
| `detect_app(path)` | Scan a folder (package.json scripts, vite/next/express, docker-compose, Procfile, Makefile) and **propose** a service config — does not save |
| `register_app(config)` / `update_app(app, config)` | Save/modify config (no source edits) |
| `start_app(app, profile?)` / `stop_app(app)` / `restart_service(app, service)` | Lifecycle |
| `get_logs(app, service, lines?)` | Tail captured logs |
| `set_env(app, service, kv)` / `set_port(app, service, port)` | Tweaks |
| `open_app(app)` | Open the served URL in the browser |

Design tools to return structured JSON (status, resolved ports, the port plan) so
Claude can reason and report. Confirm-gate destructive ones at the UI layer.

## 7. Security model

- MCP bound to `127.0.0.1` + per-install bearer token (rotatable).
- Running arbitrary commands is the whole point, but: show every command in the
  UI before first run of a new service; a "trusted apps" notion; never auto-run a
  service config that arrived from outside without a user confirming once.
- No network exposure by default. Document the surface clearly (it ships to others).

## 8. Auto-detection (where Claude shines)

`detect_app` heuristics: `package.json` scripts (`dev`/`start`/`build`),
framework signatures (vite/next/remix/express/fastify/nest), `docker-compose.yml`
services, `Procfile`, `Makefile` targets, common ports. Returns a **proposed**
config + confidence notes; the user (or their Claude, with confirmation) saves it.

## 9. Distribution & install

- Tauri bundle (`.dmg`/`.app`). Note: unsigned apps hit Gatekeeper — plan for
  **code signing + notarization** (Apple Developer ID) before sharing widely;
  until then, right-click→Open. Cross-platform later (Tauri supports win/linux;
  process-group/kill code is the main per-OS branch).
- First-run: pick app data dir, generate MCP token, show "Connect your Claude".

## 10. MVP (vertical slice to build first)

Prove the whole loop on **QuizletLocal**:
1. Register QuizletLocal with a `server` service (`node server.js`, port 4321).
2. One-button start → resolves port (4321, or next free) → spawns → health-checks
   `/` → shows "ready" + live logs. Stop kills it cleanly.
3. Host the MCP server with: `list_apps`, `app_status`, `start_app`, `stop_app`,
   `get_logs`, `detect_app`.
4. From a real Claude session: `claude mcp add` Harbor, then have Claude
   `detect_app` QuizletLocal, `start_app`, read status, `stop_app` — end to end.

If that loop works, everything else is iteration.

## 11. Milestones / build order

- **M0 — scaffold:** Tauri 2 + React app, sidebar/detail shell, app-data dir, config store.
- **M1 — orchestration:** spawn/stop with process groups, log streaming to UI, status.
- **M2 — port intelligence:** preferred→free allocation, `${...}` resolution, dependent rewiring, port-plan UI.
- **M3 — MCP server:** axum + `rmcp`, token auth, the 6 MVP tools, "Connect your Claude" UX. *(MVP = M0–M3.)*
- **M4 — UX polish:** health checks, profiles, config editor, detect_app heuristics, confirmations.
- **M5 — distribution:** signing/notarization, .dmg, docs; migrate QuizletLocal off the `.command` files onto Harbor.

## 12. Open questions for the build session

- Confirm `rmcp` (official Rust MCP SDK) supports streamable-HTTP **server** mode; else SSE or a thin axum+JSON-RPC shim.
- Config store: SQLite vs flat JSON files (lean JSON for MVP, SQLite if it grows).
- Per-project `harbor.json` committed vs central-only (support both; central is source of truth, file is import/export).
- Process-tree kill correctness on macOS (process groups) — test with node + vite children.
- Token UX for Claude Desktop vs Claude Code (both need the header/JSON snippet).

## 13. First test config (reference)

QuizletLocal, prod profile = one `server` service on 4321 (above). Dev profile
adds a `web` Vite service (port 5173) that `dependsOn` `server` and proxies
`${services.server.port}`. This single example exercises ordering, ports,
dependent rewiring, health checks, and profiles.
