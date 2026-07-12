# Security Policy

Harbor runs locally. Its embedded MCP server binds to 127.0.0.1 only and
requires a per-install bearer token. Release builds are signed and notarized
with an Apple Developer ID.

If you find a vulnerability, please report it privately instead of opening a
public issue:

- [Report a vulnerability](https://github.com/luke-fairbanks/harbor-mcp/security/advisories/new)

I'll respond within a few days. The latest release is the only supported
version.
