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
  one restart-safe pinned launcher for Claude and Codex, single-instance app
  startup, and reserved-socket MCP startup.
- Structural config validation, atomic rename/update, per-app start serialization,
  and hydration of logs emitted before the UI subscribed.
- Developer ID signing, Apple notarization, signed in-app updates with automatic
  and manual checks, progress feedback, and fail-closed release verification.

## Next: release-quality safety and distribution

1. **Bundled native MCP sidecar**
   Replace the Node-based adapter with a signed Rust stdio bridge
   that can start the Harbor background service, authenticate server identity
   over a Unix-domain socket, and work fully offline. Package the same bridge in
   an optional Codex plugin with a small Harbor workflow skill.

2. **Secret-safe environments**
   Store sensitive values in macOS Keychain, persist only references, mask them
   in the editor, redact logs/MCP/AI-fix prompts, and export `.env.example` keys
   rather than literal values.

3. **Diagnostics**
   Ship a diagnostics screen that checks runtimes, the MCP endpoint, filesystem
   permissions, update status, and client connections without exposing tokens.

4. **Approval and audit history**
   Expand the current trust gate into command/config fingerprints, change diffs,
   approval expiry/revocation, connector-specific capabilities, and an append-only
   record of who started, stopped, or changed each service.

## Then: make local systems effortless

5. **Stable human URLs**
   Add explicit primary endpoints and a loopback reverse proxy such as
   `project.localhost`, so port changes disappear for users and agents.

6. **Worktree- and agent-aware sessions**
   Resolve repository, worktree, branch, and commit from each process; tag
   Harbor-launched runs with a run/actor ID; distinguish accidental duplicates
   from intentional parallel worktrees.

7. **Persistent run history**
   Store bounded redacted logs and lifecycle events, add search/error filters and
   resource history, and produce a one-click diagnostics bundle.

8. **Docker Compose and local data services**
   Parse Compose services, ports, health checks, dependencies, and profiles;
   surface database endpoints while keeping volume deletion behind a separate,
   high-friction confirmation.

9. **Tasks and preflight checks**
   Separate one-shot install/migrate/seed/build tasks from long-running services.
   Before first start, check runtimes, dependencies, missing environment keys,
   Docker availability, and command working directories in plain language.

10. **Explicit preview sharing**
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
