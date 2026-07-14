# Harbor product roadmap

Harbor's product direction is a **trusted local runtime control plane for people
and coding agents**: one place to understand what is running, prevent accidental
duplicates, and safely operate a project without becoming a server expert.

## Shipped in this upgrade

- User-wide local listener inventory with project matching, HTTP/framework
  hints, probable-duplicate grouping, network-visible bind warnings, and an
  eight-second live refresh.
- Preferred-port discovery before allocation, including relocatable `${PORT}`
  services and every profile.
- Explicit separation between observed, matched, external, and Harbor-managed
  processes; identity-safe cleanup only for isolated process groups.
- Protections that refuse to claim or kill shells, terminals, IDEs, Claude,
  Codex, and other agent hosts. Unsafe matches still block duplicate launches.
- Local approval for configs registered through MCP, typed tool inputs, tool
  safety annotations, and discovery/cleanup MCP tools.
- Atomic owner-only state/credential files, accurate MCP health/config status,
  single-instance app startup, and reserved-socket MCP startup.
- A signed native Rust stdio sidecar for Claude Desktop, Claude Code, and Codex,
  bundled as a universal Tauri `externalBin` and atomically installed at the
  stable owner-only app-support command. It works offline, starts Harbor in the
  background, and keeps one client session connected across Harbor quit/reopen,
  token rotation, and port changes.
- Structural config validation, atomic rename/update, per-app start serialization,
  and hydration of logs emitted before the UI subscribed.
- Developer ID signing, Apple notarization, signed in-app updates with automatic
  and manual checks, progress feedback, and fail-closed release verification.

## Next: release-quality safety and distribution

1. **Secret-safe environments**
   Store sensitive values in macOS Keychain, persist only references, mask them
   in the editor, redact logs/MCP/AI-fix prompts, and export `.env.example` keys
   rather than literal values. Evaluate an owner-only Unix-domain socket as
   optional defense in depth for the bridge/backend hop; the shipped loopback
   bearer token, proxy isolation, and listener-ownership checks remain the
   compatible baseline rather than making UDS adoption a release blocker.

2. **Diagnostics**
   Ship a diagnostics screen that checks runtimes, the MCP endpoint, filesystem
   permissions, update status, and client connections without exposing tokens.

3. **Approval and audit history**
   Expand the current trust gate into command/config fingerprints, change diffs,
   approval expiry/revocation, connector-specific capabilities, and an append-only
   record of who started, stopped, or changed each service.

## Then: make local systems effortless

4. **Stable human URLs**
   Add explicit primary endpoints and a loopback reverse proxy such as
   `project.localhost`, so port changes disappear for users and agents.

5. **Worktree- and agent-aware sessions**
   Resolve repository, worktree, branch, and commit from each process; tag
   Harbor-launched runs with a run/actor ID; distinguish accidental duplicates
   from intentional parallel worktrees.

6. **Persistent run history**
   Store bounded redacted logs and lifecycle events, add search/error filters and
   resource history, and produce a one-click diagnostics bundle.

7. **Docker Compose and local data services**
   Parse Compose services, ports, health checks, dependencies, and profiles;
   surface database endpoints while keeping volume deletion behind a separate,
   high-friction confirmation.

8. **Tasks and preflight checks**
   Separate one-shot install/migrate/seed/build tasks from long-running services.
   Before first start, check runtimes, dependencies, missing environment keys,
   Docker availability, and command working directories in plain language.

9. **Explicit preview sharing**
    Offer expiring authenticated Tailscale/Cloudflare tunnels with a permanent
    visible "Public" indicator and one-click revocation. Never expose a listener
    without a direct user action.

## Product rules

- Observation is not ownership. Matching a process must never silently grant
  permission to terminate it.
- Agent-written commands require human approval before execution.
- Every destructive action carries a fresh process/config identity precondition.
- Local-first and loopback-only are defaults; public access is always explicit.
- Simple mode explains outcomes; advanced mode exposes commands and wiring.
