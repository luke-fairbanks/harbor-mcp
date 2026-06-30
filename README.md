# ⚓ Harbor

A polished, native macOS app that boots all the local servers a project needs
with **one button** — with intelligent port allocation, crash recovery, and an
**MCP server** baked in so your Claude (or Codex) can discover, configure, and
drive your dev environment without you editing a single config file.

Local process managers exist (foreman, pm2, Tilt, Herd). **None are MCP-native.**
That's Harbor's point: register a project once, and from then on an AI agent can
detect what it needs, write the run config, start/stop everything with automatic
port allocation, watch resource usage, and read logs to debug.

> Stack: **Tauri 2** (Rust core + React 19 / Radix Themes UI). macOS only for now.
> See [`DESIGN.md`](./DESIGN.md) for the architecture.

> **Note:** unrelated to the [CNCF Harbor](https://goharbor.io) container registry.

## Features

- **One-button start** with a topo-sorted dependency order and live log streaming.
- **Port intelligence** — preferred → next-free allocation probed on **both** IPv4
  and IPv6, with `${PORT}` / `${services.X.port}` placeholders resolved so a
  frontend automatically targets whatever port the backend actually got. Recognizes
  a command that pins its own port (e.g. `next dev -p 3002`) and never bumps it.
- **Detects already-running servers** — re-adopts processes a previous Harbor
  session left running, *and* recognizes a server you started yourself in a
  terminal (matched to the app by its port + project folder via a process-group
  walk), so it's never fooled into a duplicate `EADDRINUSE` start. It will never
  claim or kill an unrelated process or your shell.
- **Auto-restart on crash** (opt-in per app) with bounded backoff and a give-up
  cap, plus native crash notifications — and it can tell a deliberate Stop apart
  from a crash, so it never fights you.
- **Live resource monitor** — CPU% and memory per service, summed over the whole
  process group, in the card and the menu-bar popover.
- **Menu-bar popover** to start/stop/open your servers without opening the window.
- **Fix with AI** — surfaces a service's error and hands a tailored prompt to a
  connected Claude/Codex (or runs it headless) to diagnose it.
- **Smart onboarding** — drag a project folder onto the window and Harbor scans it
  and proposes a config (Next/Vite/Remix/Nuxt/SvelteKit/Astro/Angular/CRA/Gatsby,
  Django/FastAPI/Flask, Go, Rails, static sites; pnpm/yarn/bun aware).
- **MCP-native** — an in-process Streamable-HTTP MCP server (bound to `127.0.0.1`,
  per-install bearer token) exposes the whole lifecycle to an agent.

## Run it

```bash
npm install
npm run tauri dev      # dev window + MCP server on 127.0.0.1:7777
npm run tauri build    # produces a distributable .app / .dmg
```

The `.app`/`.dmg` are **unsigned** — first launch is right-click → Open
(Gatekeeper). Code signing + notarization is the remaining step for wide
distribution.

## Connect your Claude (or Codex)

Open the **Connect your Claude** screen (gear, bottom-left) for one-click setup
(Claude Code, Claude Desktop, and Codex), or run it manually with your token from
`~/Library/Application Support/com.harbor.desktop/mcp.json`:

```bash
claude mcp add --transport http harbor http://127.0.0.1:7777/mcp \
  --header "Authorization: Bearer <token>"
```

Then ask your agent to `detect_app` a folder, `start_app` it, read `app_status` /
`get_logs`, `restart_app`, and `stop_app`.

## Concepts

- **App** — a registered project folder with a name, root, and services.
- **Service** — one long-running process: `{ name, cwd, command, port?, env,
  dependsOn[], healthCheck?, readyLogPattern? }`. `command`/`env` may contain
  `${PORT}` and `${services.X.port}` placeholders.
- **Profile** — a named service set (e.g. `default` = just the server, `dev` =
  server + web).
- **Port plan** — what each service preferred vs. the port it actually got, and
  how dependents were rewired.

## MCP tools

| Tool | Purpose |
|------|---------|
| `list_apps` | Registered apps + current run status |
| `app_status(app)` | Per-service state, resolved ports, the port plan |
| `detect_app(path)` | Scan a folder, **propose** a config (does not save) |
| `register_app(config)` | Add/replace an app in the registry |
| `start_app(app, profile?)` / `stop_app(app)` / `restart_app(app, profile?)` | Lifecycle |
| `get_logs(app, service, lines?)` | Tail captured logs |

The server binds `127.0.0.1` only and requires a per-install bearer token.

## Module map (`src-tauri/src`)

| File | Responsibility |
|------|----------------|
| `model.rs` | Config + run types (serde) |
| `store.rs` | Flat-JSON registry, MCP settings, and the `runs.json` adoption record |
| `state.rs` | `AppState` shared by commands and the MCP server |
| `supervisor.rs` | Spawn in process groups, stream logs, health/ready, adoption, auto-restart, resource sampling, clean kill |
| `ports.rs` | Topo sort, dual-stack allocation, pinned-port detection, `${...}` resolution (unit-tested) |
| `health.rs` | TCP/HTTP readiness probes |
| `detect.rs` | `detect_app` framework heuristics (unit-tested) |
| `ops.rs` | Shared start/stop/restart/open logic (commands + MCP) |
| `commands.rs` | Tauri command surface |
| `mcp.rs` | axum + `rmcp` Streamable-HTTP server, bearer auth |
| `lib.rs` | Wires it together; hosts the MCP server on Tauri's runtime |

## Contributing

Issues and PRs welcome. The Rust core has unit + integration tests:

```bash
cd src-tauri && cargo test
npx tsc --noEmit   # from the repo root, typecheck the UI
```

## License

[MIT](./LICENSE) © Luke Fairbanks
