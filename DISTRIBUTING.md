# Distributing Harbor (signing, notarization, releases)

Harbor is a developer tool that spawns arbitrary processes, so it **can't** go on
the Mac App Store (the App Sandbox forbids exactly what Harbor does). The right
path — the same one OrbStack, Docker Desktop, Warp, etc. use — is a **Developer
ID–signed, Apple-notarized** build distributed directly (DMG + GitHub Releases).

The [`Release` workflow](.github/workflows/release.yml) does the build, signing,
notarization, and release automatically when you push a version tag. You just do
the one-time credential setup below.

> **Signing is optional.** Notarization is *not* the App Store — it's a free Apple
> malware scan that removes the Gatekeeper warning, and it needs the $99/yr
> Developer ID. If you skip all of it, the **same workflow still produces a working
> unsigned `.dmg`** — just push a tag without adding any `APPLE_*` secrets. Users
> then click through a one-time *System Settings → Privacy & Security → Open Anyway*
> on first launch, which is normal for an open-source dev tool. Add the secrets
> later to upgrade to a notarized build with zero rework.

---

## 1. Create a Developer ID Application certificate

You need an **Apple Developer Program** membership (you have one).

Easiest path (Xcode): **Xcode → Settings → Accounts → (your team) → Manage
Certificates → ＋ → Developer ID Application**. This creates the cert and stores
its private key in your login keychain.

(Portal path: developer.apple.com → Certificates → ＋ → *Developer ID
Application* → upload a CSR from Keychain Access → download → double-click to
install.)

## 2. Gather the values you'll need

| Value | How to get it |
|---|---|
| **`APPLE_SIGNING_IDENTITY`** | Run `security find-identity -v -p codesigning` and copy the full quoted name, e.g. `Developer ID Application: Luke Fairbanks (ABCDE12345)`. |
| **`APPLE_TEAM_ID`** | The 10-character Team ID from [developer.apple.com/account](https://developer.apple.com/account) → Membership. (Also the part in parentheses above.) |
| **`APPLE_ID`** | Your Apple Developer account email. |
| **`APPLE_PASSWORD`** | An **app-specific password** (not your real password): [account.apple.com](https://account.apple.com) → Sign-In & Security → App-Specific Passwords → ＋. Looks like `abcd-efgh-ijkl-mnop`. |
| **`APPLE_CERTIFICATE`** | Export the cert **with its private key** from Keychain Access (right-click → Export → `.p12`, set an export password), then base64 it: `base64 -i Certificates.p12 \| pbcopy`. |
| **`APPLE_CERTIFICATE_PASSWORD`** | The export password you set on the `.p12`. |
| **`KEYCHAIN_PASSWORD`** | Any random string — it's only the password for the throwaway keychain CI creates. `openssl rand -base64 24`. |

> These are secrets. Don't commit them or paste them anywhere but GitHub's
> encrypted secrets UI (below). Notarization is free; only the membership costs.

## 3. Add them as GitHub Actions secrets

Repo → **Settings → Secrets and variables → Actions → New repository secret** —
add one for each name in the table above (exact names matter):

```
APPLE_SIGNING_IDENTITY   APPLE_TEAM_ID   APPLE_ID   APPLE_PASSWORD
APPLE_CERTIFICATE   APPLE_CERTIFICATE_PASSWORD   KEYCHAIN_PASSWORD
```

## 4. Cut a release

Keep the tag in sync with `version` in `src-tauri/tauri.conf.json`, then push it:

```bash
git tag v0.2.1
git push origin v0.2.1
```

The workflow builds a universal `.dmg`, signs + notarizes it, and creates a
**draft** GitHub Release with the `.dmg` attached. Review it and click *Publish*.
(To auto-publish instead, set `releaseDraft: false` in the workflow.)

Verify a downloaded build locally if you want:

```bash
spctl -a -vvv -t install /path/to/Harbor.app   # → "accepted, source=Notarized Developer ID"
xcrun stapler validate /path/to/Harbor.app
```

---

## Building a signed `.dmg` locally (no CI)

With the cert in your login keychain:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Luke Fairbanks (ABCDE12345)"
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="abcd-efgh-ijkl-mnop"
export APPLE_TEAM_ID="ABCDE12345"

npm run tauri build -- --target universal-apple-darwin
```

The signed, notarized `.dmg` lands in
`src-tauri/target/universal-apple-darwin/release/bundle/dmg/`.

## Homebrew (after each release)

The cask lives in the shared tap
[`luke-fairbanks/homebrew-tap`](https://github.com/luke-fairbanks/homebrew-tap)
(`Casks/harbor.rb`, alongside Battery Hog's cask). After each release, bump
`version` and `sha256` there:

```bash
shasum -a 256 Harbor_<version>_universal.dmg
```

Anyone can then `brew install --cask luke-fairbanks/tap/harbor`.

## A note on entitlements

Harbor runs **without the App Sandbox** and needs no custom entitlements: it's
signed with the Hardened Runtime (Tauri's default when signing), the child dev
servers it spawns run as their own processes, and the webview's JIT runs in
Apple's own already-signed WebKit process. If a future notarization run ever
flags something, add an `entitlements` plist and point `bundle.macOS.entitlements`
at it — but you almost certainly won't need to.

## MCP distribution behavior

Harbor's Streamable-HTTP server is part of the signed app and binds only to
loopback. During startup it reserves the selected socket before the UI and agent
configuration advertise it, eliminating the old port check/bind race.

- Harbor's one-click setup for Codex, Claude Code, and Claude Desktop writes an
  owner-only launcher beside `mcp.json`. The launcher reads the current protected
  port/per-launch token at each client start, opens Harbor quietly if needed,
  and then runs the pinned `mcp-remote@0.1.38` adapter. Native HTTP configuration remains
  available for advanced/manual setups, but requires Harbor to be open and must
  be refreshed after each Harbor restart.
- Harbor is single-instance: launching it again focuses the existing window
  instead of allowing two processes to race the endpoint descriptor.
- App data is `0700`; `mcp.json`, registry/run state, agent configs, and Harbor's
  safety backups are written atomically as `0600`.
- Client status is based on the current URL/token or launcher descriptor, not
  merely the presence of an entry named `harbor`.

The current restart-safe bridge used by Claude Code, Claude Desktop, and Codex
still needs Node/npx and may need network on its first run. Manual native HTTP
configuration avoids that dependency but requires Harbor to be open and the
client entry to match its current port. A future fully offline release should
replace the bridge with a signed Rust stdio sidecar bundled inside `Harbor.app`;
see `ROADMAP.md`.
