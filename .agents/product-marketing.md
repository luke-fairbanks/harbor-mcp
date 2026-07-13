# Harbor product marketing context

Last updated: July 13, 2026

## Product

Harbor is a Mac control center for local development. It inventories local TCP
servers owned by the current macOS user, maps strong matches to registered
projects, flags likely duplicate runs, and lets people start and monitor a
project's services from one place. An embedded MCP server gives Claude and Codex
the same runtime view and lifecycle tools.

Harbor is free, open source under MIT, macOS 11+ only, universal for Apple
Silicon and Intel, signed by Faba Development, notarized by Apple, and delivered
through signed user-approved updates.

## Primary audience

Mac users building local apps with Claude, Codex, Cursor, or other coding agents
who now have more projects, terminals, ports, and background servers than they
can confidently manage. Many can create software faster than they have learned
process and port operations.

Their language:

- "Which copy of my app is current?"
- "Why is it on 3004 now?"
- "Can I close this terminal?"
- "What is still running?"
- "Can the agent fix this without starting another copy?"

Secondary audiences are indie hackers and professional engineers running
multi-service projects, plus MCP/local-first power users who care about agent
safety.

## Positioning

Category: **The Mac control center for AI-assisted local development.**

Headline: **Know what's running. Stop the port chaos.**

Promise: Harbor turns a Mac full of mystery listeners into a visible set of
projects that a person or coding agent can operate through explicit safety
boundaries.

One sentence: Harbor inventories running local servers, maps strong project
matches, flags likely duplicates, starts full stacks with smart ports, and lets
Claude or Codex operate approved projects over MCP.

## Message hierarchy

1. **Know what is running.** See ports, commands, folders, HTTP clues, project
   matches, and likely duplicates.
2. **Run each project as one thing.** Start a whole stack with dependency order,
   resolved ports, logs, resources, and crash recovery.
3. **Give agents the same source of truth.** Claude and Codex can inspect and
   operate Harbor over MCP. Project configs created through MCP need local
   approval before Harbor can run their commands.
4. **Trust the tool.** Local-first, no account, loopback-only MCP, per-launch
   token, identity-checked cleanup, signed/notarized app, signed updater, and
   public source.

## Differentiation

Do not claim that competing process managers lack MCP. Laravel Herd and PM2 now
advertise MCP integrations.

Differentiate Harbor on the combination of:

- current-user local listener inventory across stacks;
- evidence-based project matching and likely-duplicate warnings;
- adoption of matching already-running servers before port allocation;
- explicit separation between observation and process ownership;
- refusal to offer cleanup for terminal-, IDE-, agent-, shell-, or
  Harbor-hosted process groups;
- one Mac UI for full-stack lifecycle, logs, resource status, and menu-bar
  control;
- embedded MCP lifecycle with local approval for MCP-created project configs.

## Claim boundaries

- Harbor inventories current-user TCP listeners, not every process on the Mac.
- A match is evidence-based and best-effort. Say "maps strong matches," not
  "always finds the right project."
- "Safe to stop" refers to Harbor's identity and isolation checks. It does not
  guarantee that a process has no unsaved state or workflow impact.
- Config approval is one-time for the exact MCP-created project config. Approved
  lifecycle calls do not trigger a fresh in-app prompt for every action.
- The restart-safe MCP bridge currently requires Node.js/npx and may fetch the
  pinned `mcp-remote@0.1.38` adapter on first use. A manual session-scoped HTTP
  setup exists; a bundled native bridge is roadmap work.
- Do not claim Docker Compose, public tunnels, stable `.localhost` URLs,
  worktree-aware sessions, Windows, or Linux support. Those are roadmap items.
- Harbor is unrelated to the CNCF Harbor container registry.

## Voice

Calm, direct, technically honest, and accessible. Lead with a recognizable
problem. Use MCP as supporting proof for non-technical audiences, not the first
term they must decode. Avoid "ultimate," "revolutionary," "game-changing," and
fragile competitive absolutes.

## Primary actions

1. Download the signed DMG from the latest GitHub release.
2. Install through `brew install --cask luke-fairbanks/tap/harbor`.
3. Add a real project and inspect Local Servers.
4. Optionally connect Claude or Codex.
5. Report first-run problems through GitHub Issues without tokens or secrets.

## Canonical links

- Repository: <https://github.com/luke-fairbanks/harbor-mcp>
- Latest release: <https://github.com/luke-fairbanks/harbor-mcp/releases/latest>
- Roadmap: <https://github.com/luke-fairbanks/harbor-mcp/blob/main/ROADMAP.md>
- Security reports:
  <https://github.com/luke-fairbanks/harbor-mcp/security/advisories/new>
