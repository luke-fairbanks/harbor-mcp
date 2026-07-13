# Distributing Harbor (signing, notarization, releases)

Harbor is a developer tool that spawns arbitrary processes, so it **can't** go on
the Mac App Store (the App Sandbox forbids exactly what Harbor does). The right
path — the same one OrbStack, Docker Desktop, Warp, etc. use — is a **Developer
ID–signed, Apple-notarized** build distributed directly (DMG + GitHub Releases).

The [`Release` workflow](.github/workflows/release.yml) builds, signs, notarizes,
and verifies each release when a matching version tag is pushed. Production
releases are fail-closed: the workflow completes successfully only after both
the Apple build and updater artifact pass their signature checks. Drafts must
remain unpublished unless every workflow gate is green.

## How in-app updates behave

Production builds check the public GitHub update feed shortly after launch and
every six hours. An automatic check is quiet when Harbor is current or the feed
is temporarily unreachable. Users can bypass the schedule at any time from
**Settings → Harbor updates → Check for updates**.

Harbor never installs an update silently:

- when a newer signed version exists, Harbor shows its version and release-note
  summary and waits for the user to choose **Update and restart**;
- **Later** hides that same version from automatic prompts for 24 hours, while a
  manual check still reveals it immediately;
- the updater verifies the archive with Harbor's embedded updater public key
  before replacing the app; the release pipeline separately verifies Faba
  Development's Developer ID signature, Gatekeeper acceptance, and Apple
  notarization;
- Harbor relaunches after installation. Managed project processes remain online
  during the Harbor app restart;
- `npm run tauri dev` does not perform automatic checks. The
  `tauri:build:local` package omits updater artifacts, but it is still a
  production build and will check the configured public feed when launched.

> Harbor v0.3.0 and earlier do not contain the updater. Those users must install
> the current release (v0.4.0 or later) manually once. Every signed release after
> that can update in-app.

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
| **`APPLE_SIGNING_IDENTITY`** | Run `security find-identity -v -p codesigning` and copy the full quoted name, e.g. `Developer ID Application: Faba Development LLC (M58C5Q8BJC)`. |
| **`APPLE_TEAM_ID`** | The 10-character Team ID from [developer.apple.com/account](https://developer.apple.com/account) → Membership. (Also the part in parentheses above.) |
| **`APPLE_ID`** | Your Apple Developer account email. |
| **`APPLE_PASSWORD`** | An **app-specific password** (not your real password): [account.apple.com](https://account.apple.com) → Sign-In & Security → App-Specific Passwords → ＋. Looks like `abcd-efgh-ijkl-mnop`. |
| **`APPLE_CERTIFICATE`** | Export the cert **with its private key** from Keychain Access (right-click → Export → `.p12`, set an export password), then base64 it: `base64 -i Certificates.p12 \| pbcopy`. |
| **`APPLE_CERTIFICATE_PASSWORD`** | The export password you set on the `.p12`. |
| **`KEYCHAIN_PASSWORD`** | Any random string — it's only the password for the throwaway keychain CI creates. `openssl rand -base64 24`. |

> These are secrets. Don't commit them or paste them anywhere but GitHub's
> encrypted secrets UI (below). Notarization is free; only the membership costs.

## 3. Create and protect the updater key

Generate this key once. Losing or replacing it prevents existing Harbor installs
from trusting future updates.

```bash
password=$(openssl rand -base64 32)
npm run tauri -- signer generate \
  --write-keys ~/.tauri/harbor-updater.key \
  --password "$password"
```

- Back up `~/.tauri/harbor-updater.key` and its password in separate secure
  locations. Never commit either one.
- Put the complete contents of `harbor-updater.key` in the
  `TAURI_SIGNING_PRIVATE_KEY` GitHub secret.
- Put its password in `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
- Put the complete contents of `harbor-updater.key.pub` in
  `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`. The public key is safe
  to commit.

The current Faba Development updater key is backed up in the macOS login
Keychain under `Harbor Updater Private Key` and `Harbor Updater Signing`, and in
the two GitHub Actions secrets above.

## 4. Add the Apple credentials as GitHub Actions secrets

Repo → **Settings → Secrets and variables → Actions → New repository secret** —
add one for each name in the table above (exact names matter):

```
APPLE_SIGNING_IDENTITY   APPLE_TEAM_ID   APPLE_ID   APPLE_PASSWORD
APPLE_CERTIFICATE   APPLE_CERTIFICATE_PASSWORD   KEYCHAIN_PASSWORD
```

The release workflow requires all seven Apple values plus both updater values.
It stops before packaging or signing if any one is missing.

## 5. Cut and publish a release

You need authenticated `git` and [GitHub CLI](https://cli.github.com/) access,
plus `jq`. Use a new SemVer version greater than every published Harbor version:
a patch for fixes or release canaries, a minor version for backward-compatible
features, and a major version for intentionally breaking behavior or formats.
Never reuse or move a version tag after pushing it.

### 5.1 Prepare the version on `main`

Start from a clean, current `main`. Replace `<X.Y.Z>` before running the rest of
the commands; `VERSION` never includes the leading `v`.

```bash
git switch main
git pull --ff-only
test -z "$(git status --porcelain)"

export VERSION="<X.Y.Z>"
test "$VERSION" != "<X.Y.Z>"
test -z "$(git tag --list "v${VERSION}")"
test -z "$(git ls-remote --tags origin "refs/tags/v${VERSION}")"
```

Update these exact values:

- `package.json` → top-level `version`;
- `package-lock.json` → top-level `version` and `packages[""].version`;
- `src-tauri/Cargo.toml` → `[package].version`;
- `src-tauri/Cargo.lock` → the `version` in the `[[package]]` block whose name is
  `harbor`;
- `src-tauri/tauri.conf.json` → top-level `version`.

The following local check mirrors the workflow's version gate:

```bash
VERSION="$VERSION" node <<'NODE'
const fs = require("fs");
const packageJson = JSON.parse(fs.readFileSync("package.json", "utf8"));
const packageLock = JSON.parse(fs.readFileSync("package-lock.json", "utf8"));
const tauri = JSON.parse(fs.readFileSync("src-tauri/tauri.conf.json", "utf8"));
const cargoToml = fs.readFileSync("src-tauri/Cargo.toml", "utf8");
const cargoLock = fs.readFileSync("src-tauri/Cargo.lock", "utf8");
const actual = {
  package: packageJson.version,
  packageLock: packageLock.version,
  packageLockRoot: packageLock.packages?.[""]?.version,
  tauri: tauri.version,
  cargo: cargoToml.match(/^version = "([^"]+)"/m)?.[1],
  cargoLock: cargoLock.match(
    /\[\[package\]\]\nname = "harbor"\nversion = "([^"]+)"/,
  )?.[1],
};
console.table(actual);
if (Object.values(actual).some((value) => value !== process.env.VERSION)) {
  process.exit(1);
}
NODE
```

Run the same substantive checks that CI will run, then commit and push `main`
before creating the tag:

```bash
npm ci
npm test
npm run build

(
  cd src-tauri
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets -- -D warnings
  cargo test --locked
)

git diff --check
git add package.json package-lock.json \
  src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "chore: release Harbor v${VERSION}"
git push origin main
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
```

### 5.2 Trigger and monitor the protected release

Create an annotated tag on the exact commit now present on `origin/main`, then
push only that tag:

```bash
git tag -a "v${VERSION}" -m "Harbor v${VERSION}"
git push origin "v${VERSION}"

sleep 3
RUN_ID="$(gh run list --workflow Release --branch "v${VERSION}" --limit 1 \
  --json databaseId --jq '.[0].databaseId')"
test -n "$RUN_ID"
gh run watch "$RUN_ID" --exit-status --interval 10
```

If the run does not appear after three seconds, rerun the `RUN_ID=...` command.
Tags not reachable from `origin/main` are rejected. The workflow runs the full
frontend and Rust test suites, builds a universal app, signs and notarizes the
app and DMG, and creates a **draft** GitHub Release containing:

- `Harbor_<version>_universal.dmg` for manual installation;
- `Harbor_<version>_universal.app.tar.gz` and `.sig` for in-app updates;
- `latest.json`, mapping both Intel and Apple Silicon Macs to the universal
  updater artifact.

The final workflow step downloads the draft updater archive and checks its
signature, Apple signature, Gatekeeper acceptance, and notarization ticket. If
the run fails, do not publish the draft.

### 5.3 Inspect and publish the draft

Before publication, confirm the draft points at the tag commit and contains
exactly the expected four assets:

```bash
test "$(gh release view "v${VERSION}" --json targetCommitish \
  --jq .targetCommitish)" = "$(git rev-parse "v${VERSION}^{}")"

RELEASE_JSON="$(mktemp)"
gh release view "v${VERSION}" \
  --json isDraft,isPrerelease,assets,url > "$RELEASE_JSON"
jq -e --arg version "$VERSION" '
  .isDraft == true and
  .isPrerelease == false and
  ([.assets[].name] | sort) ==
    ([
      "Harbor_\($version)_universal.app.tar.gz",
      "Harbor_\($version)_universal.app.tar.gz.sig",
      "Harbor_\($version)_universal.dmg",
      "latest.json"
    ] | sort)
' "$RELEASE_JSON"
```

Review the workflow log and generated release notes. Only then make the release
public; this is the moment GitHub's `/releases/latest/download/latest.json`
endpoint switches the live updater feed:

```bash
gh release edit "v${VERSION}" --draft=false
```

### 5.4 Verify the public feed and smoke-test the updater

Verify that an unauthenticated user sees the intended version and non-empty
download URLs/signatures for both supported Mac architectures:

```bash
LATEST_JSON="/tmp/harbor-latest-${VERSION}.json"
curl --retry 5 --retry-all-errors -fsSL \
  "https://github.com/luke-fairbanks/harbor-mcp/releases/latest/download/latest.json" \
  -o "$LATEST_JSON"

jq -e --arg version "$VERSION" '
  . as $release |
  ($release.version == $version) and
  ([
    "darwin-aarch64-app",
    "darwin-x86_64-app",
    "darwin-aarch64",
    "darwin-x86_64"
  ] | all(
    . as $target |
    (($release.platforms[$target].url | type) == "string") and
    (($release.platforms[$target].url | length) > 0) and
    (($release.platforms[$target].signature | type) == "string") and
    (($release.platforms[$target].signature | length) > 0)
  ))
' "$LATEST_JSON"

curl -fsSIL \
  "https://github.com/luke-fairbanks/harbor-mcp/releases/download/v${VERSION}/Harbor_${VERSION}_universal.dmg"
```

Keep a signed copy of the immediately previous Harbor version installed for the
end-to-end smoke test. Do not install the new DMG manually:

1. Open the older Harbor build and confirm its version in **Settings**.
2. Choose **Check for updates**, then **Update to vX.Y.Z** (or **Update and
   restart** in the global notice).
3. Confirm Harbor downloads, installs, and relaunches without stopping managed
   project processes.
4. Return to **Settings** and confirm the new version. A second manual check must
   report that Harbor is current.

After the updater smoke test passes, complete the [Homebrew cask
update](#homebrew-after-each-release) so fresh Homebrew installations receive the
same version.

For an additional local signature and Gatekeeper check on an extracted app:

```bash
codesign --verify --deep --strict --verbose=2 /path/to/Harbor.app
spctl -a -vvv -t exec /path/to/Harbor.app   # → "accepted, source=Notarized Developer ID"
xcrun stapler validate /path/to/Harbor.app
```

### 5.5 Failure, rollback, and key recovery

- **A workflow fails before publication:** the public updater feed remains on the
  previous release. Never publish a partial draft. A transient external failure
  (for example, Apple notarization downtime) can be rerun on the unchanged tag
  with `gh run rerun "$RUN_ID"`, followed by
  `gh run watch "$RUN_ID" --exit-status --interval 10`. If code or metadata must
  change, commit the fix and use a higher patch version; never force-push or move
  the existing tag.
- **A bad release is published:** Harbor does not support an in-app downgrade.
  If continued distribution is dangerous, immediately unpublish it with
  `gh release edit "v${BAD_VERSION}" --draft=true`, then fix forward with a
  higher patch version. Turning it back into a draft stops new uptake but does
  not roll back machines that already installed it.
- **The updater private key is lost:** recover it and its password from the
  documented backups. Without that key, existing installs cannot trust a new
  in-app update. A replacement key requires a new manually installed DMG that
  embeds the replacement public key.
- **The updater private key is compromised:** unpublish the affected live
  release/feed, rotate the key and GitHub secrets, and require a manual signed
  DMG bootstrap. Do not use the compromised key to claim a secure rotation.
- **Apple credentials expire:** replace the affected GitHub secret and rerun the
  unchanged failed workflow. Changing the Apple credentials does not require an
  updater-key rotation.

---

## Local builds

A normal local package does not need the updater private key:

```bash
npm run tauri:build:local
```

That override disables updater artifact creation while retaining the normal
app/DMG bundle. It does not disable the runtime updater: the packaged app is a
production build and checks Harbor's public feed when launched. Use
`npm run tauri dev` when you need a development session without automatic
update checks.

To reproduce the signed app and updater artifacts locally, load both signing
systems:

With the cert in your login keychain:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Faba Development LLC (M58C5Q8BJC)"
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="abcd-efgh-ijkl-mnop"
export APPLE_TEAM_ID="M58C5Q8BJC"
export TAURI_SIGNING_PRIVATE_KEY="$(<~/.tauri/harbor-updater.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(security find-generic-password \
  -a "$USER" -s 'Harbor Updater Signing' -w)"

npm run tauri -- build --target universal-apple-darwin
```

Tauri signs, notarizes, and staples `Harbor.app` before creating the DMG, but the
finished outer DMG still needs its own submission and stapling pass to reproduce
the release workflow:

```bash
VERSION="$(node -p 'require("./package.json").version')"
DMG="src-tauri/target/universal-apple-darwin/release/bundle/dmg/Harbor_${VERSION}_universal.dmg"

test -f "$DMG"
xcrun notarytool submit "$DMG" \
  --apple-id "$APPLE_ID" \
  --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" \
  --wait
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
spctl -a -vvv -t open --context context:primary-signature "$DMG"
```

The finished DMG is under `bundle/dmg/`; the signed updater archive and `.sig`
are beside `Harbor.app` under `bundle/macos/`. A local build does not create or
publish a GitHub Release—only the protected workflow does that.

## Homebrew (after each release)

The cask lives in the shared tap
[`luke-fairbanks/homebrew-tap`](https://github.com/luke-fairbanks/homebrew-tap)
(`Casks/harbor.rb`, alongside Battery Hog's cask). After each release, update it
from a clean tap checkout. This assumes `VERSION` is still the published version
without a leading `v`:

```bash
: "${VERSION:?Set VERSION to the published X.Y.Z without a leading v}"

brew tap luke-fairbanks/tap
TAP_DIR="$(brew --repo luke-fairbanks/tap)"
git -C "$TAP_DIR" switch main
git -C "$TAP_DIR" pull --ff-only
test -z "$(git -C "$TAP_DIR" status --porcelain)"

DMG_DIR="$(mktemp -d)"
gh release download "v${VERSION}" \
  --repo luke-fairbanks/harbor-mcp \
  --pattern "Harbor_${VERSION}_universal.dmg" \
  --dir "$DMG_DIR"
DMG="$DMG_DIR/Harbor_${VERSION}_universal.dmg"
SHA256="$(shasum -a 256 "$DMG" | awk '{print $1}')"
printf 'version=%s\nsha256=%s\n' "$VERSION" "$SHA256"
```

Edit `$TAP_DIR/Casks/harbor.rb` so its `version` and `sha256` exactly match those
two values, then validate and publish the cask change:

```bash
CASK="$TAP_DIR/Casks/harbor.rb"
brew style "$CASK"
brew audit --cask --strict --online luke-fairbanks/tap/harbor
brew fetch --cask --force --retry luke-fairbanks/tap/harbor
git -C "$TAP_DIR" diff --check

git -C "$TAP_DIR" add Casks/harbor.rb
git -C "$TAP_DIR" commit -m "harbor ${VERSION}"
git -C "$TAP_DIR" push origin main
```

Fresh installations can then use
`brew install --cask luke-fairbanks/tap/harbor` at the same version as the live
in-app update feed.

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
- AI connections recognizes Harbor's current managed launcher configuration,
  not merely an entry named `harbor`. It reports configuration separately from
  an observed **Bridge running** process and flags clients that predate the
  current config or per-launch endpoint descriptor. A bridge process alone does
  not claim that the host accepted every tool schema.

The current restart-safe bridge used by Claude Code, Claude Desktop, and Codex
still needs Node/npx and may need network on its first run. Manual native HTTP
configuration avoids that dependency but requires Harbor to be open and the
client entry to match its current port. A future fully offline release should
replace the bridge with a signed Rust stdio sidecar bundled inside `Harbor.app`;
see `ROADMAP.md`.

For any release that changes MCP schemas, transport, authentication, or the
launcher, run the release candidate with one-click client setup already present,
fully restart Claude Desktop or start a fresh Codex session, and execute:

```bash
node scripts/mcp-bridge-soak.mjs --duration-ms 90000 --interval-ms 30000
```

The harness uses the exact installed stdio launcher, validates the complete tool
catalog against Claude Desktop's object-schema requirement, and calls the
read-only `list_apps` tool immediately and after 30, 60, and 90 seconds. Do not
publish unless it ends with `PASS` and emits no schema, authentication, SSE, or
reconnect errors.
