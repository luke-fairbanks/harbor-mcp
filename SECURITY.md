# Security Policy

Harbor runs locally. Its embedded MCP server binds to 127.0.0.1 only and
requires a per-launch bearer token.

Production releases use two independent trust layers:

- Harbor's updater archives are signed with the dedicated updater signing key.
  The app verifies that signature with its embedded public key before installing
  an update.
- The app and disk image are signed with Faba Development's Apple Developer ID,
  notarized by Apple, and stapled so macOS can verify their origin and integrity.

The production release workflow fails closed. It requires both signing systems,
then verifies the updater signature, Developer ID signature, Gatekeeper
assessment, and notarization. Unsigned or otherwise unverifiable artifacts fail
the workflow and must remain an unpublished draft.

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
