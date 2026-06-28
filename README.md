# ⚓ Harbor

A native desktop app that boots all the local servers an app needs with **one
button**, manages ports intelligently, and exposes an **MCP server** so any
user's Claude can discover, configure, and drive it — without editing source.

Local process managers exist (foreman, pm2, Tilt, Herd). **None are MCP-native.**
That's Harbor's point: install it, register it with your Claude once, and from
then on Claude can detect what a project needs, write the run config, start/stop
everything with automatic port allocation, and read logs to debug.

> Stack: **Tauri 2** (Rust core + React 19 / Radix Themes UI). See
> [`DESIGN.md`](./DESIGN.md) for the full design.

## Run it

```bash
npm install
npm run tauri dev      # dev window + MCP server on 127.0.0.1:7777
npm run tauri build    # produces a distributable .app / .dmg
```

First run seeds **QuizletLocal** (if present at `~/Desktop/QuizletLocal`) as a demo.

## Connect your Claude

Open the **Connect your Claude** screen (gear, bottom-left) and copy the command
— or run it with your token from
`~/Library/Application Support/com.harbor.desktop/mcp.json`:

```bash
claude mcp add --transport http harbor http://127.0.0.1:7777/mcp \
  --header "Authorization: Bearer <token>"
```

Then ask Claude to `detect_app` a folder, `start_app` it, read `app_status` /
`get_logs`, and `stop_app`.

## Concepts

- **App** — a registered project folder with a name, root, and services.
- **Service** — one long-running process: `{ name, cwd, command, port?, env,
  dependsOn[], healthCheck?, readyLogPattern? }`. `command`/`env` may contain
  `${PORT}` and `${services.X.port}` placeholders.
- **Profile** — a named service set (e.g. `default` = just the server, `dev` =
  server + web).
- **Port plan** — what each service preferred vs. the port it actually got
  (next free if the preferred one was taken), and how dependents were rewired.

## Port intelligence

On start, services are topologically sorted by `dependsOn`, each gets its
preferred port (or the next free one — probed on **both** IPv4 and IPv6 so an
`[::]`-bound holder is detected), and `${...}` placeholders resolve against the
final plan so a frontend automatically targets whatever port the backend got.
Ports free on stop.

## MCP tools

| Tool | Purpose |
|------|---------|
| `list_apps` | Registered apps + current run status |
| `app_status(app)` | Per-service state, resolved ports, the port plan |
| `detect_app(path)` | Scan a folder, **propose** a config (does not save) |
| `start_app(app, profile?)` / `stop_app(app)` | Lifecycle |
| `get_logs(app, service, lines?)` | Tail captured logs |

The server binds `127.0.0.1` only and requires a per-install bearer token.

## Module map (`src-tauri/src`)

| File | Responsibility |
|------|----------------|
| `model.rs` | Config + run types (serde) |
| `store.rs` | Flat-JSON registry + MCP settings; QuizletLocal seed |
| `state.rs` | `AppState` shared by commands and the MCP server |
| `supervisor.rs` | Spawn in process groups, stream logs, health/ready, clean kill |
| `ports.rs` | Topo sort, dual-stack allocation, `${...}` resolution (unit-tested) |
| `health.rs` | TCP/HTTP readiness probes |
| `detect.rs` | `detect_app` heuristics |
| `ops.rs` | Shared start/stop/open logic (commands + MCP) |
| `commands.rs` | Tauri command surface |
| `mcp.rs` | axum + `rmcp` 1.8 Streamable-HTTP server, bearer auth, 6 tools |
| `lib.rs` | Wires it together; hosts the MCP server on Tauri's runtime |

## Status

MVP (M0–M3) complete and verified end-to-end on QuizletLocal. Next: config-editor
UI, richer `detect_app`, `harbor.json` import/export, code signing + notarization.
