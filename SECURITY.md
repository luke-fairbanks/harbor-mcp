# Security Policy

Harbor runs locally. Its embedded MCP server binds to 127.0.0.1 only and
requires a per-launch bearer token. The release workflow supports Apple
Developer ID signing and notarization when the repository's signing secrets are
configured; unsigned artifacts are identified in their release notes.

On shared Macs, prefer Harbor's one-click launcher. It checks that the listener
is owned by the current user before forwarding credentials, and Harbor rotates
the token at every launch. Manual native-HTTP clients send a bearer header over
plain loopback and do not authenticate the server process; their entries are
session-scoped and should be refreshed after Harbor restarts.

If you find a vulnerability, please report it privately instead of opening a
public issue:

- [Report a vulnerability](https://github.com/luke-fairbanks/harbor-mcp/security/advisories/new)

Please include the Harbor version, macOS version, reproduction steps, and the
smallest non-sensitive diagnostic sample that demonstrates the issue.
