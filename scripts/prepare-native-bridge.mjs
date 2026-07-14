#!/usr/bin/env node

import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  rmSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const crate = join(root, "src-tauri", "mcp-bridge");
const manifest = join(crate, "Cargo.toml");
const outputDir = join(root, "src-tauri", "binaries");
const binaryBase = "harbor-mcp-bridge";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    env: {
      ...process.env,
      MACOSX_DEPLOYMENT_TARGET:
        process.env.MACOSX_DEPLOYMENT_TARGET || "11.0",
      ...options.env,
    },
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    if (options.capture && result.stderr) process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return options.capture ? result.stdout.trim() : "";
}

function hostTriple() {
  const version = run("rustc", ["-vV"], { capture: true });
  const host = version.match(/^host:\s*(.+)$/m)?.[1];
  if (!host) throw new Error("could not determine the Rust host target");
  return host;
}

function buildTarget(target, release) {
  const args = [
    "build",
    "--manifest-path",
    manifest,
    "--locked",
    "--target",
    target,
  ];
  if (release) args.push("--release");
  run("cargo", args);

  const profile = release ? "release" : "debug";
  const source = join(crate, "target", target, profile, binaryBase);
  const destination = join(outputDir, `${binaryBase}-${target}`);
  copyFileSync(source, destination);
  chmodSync(destination, 0o755);
  return destination;
}

mkdirSync(outputDir, { recursive: true });

const requested =
  process.env.HARBOR_BRIDGE_TARGET ||
  process.env.TAURI_ENV_TARGET_TRIPLE ||
  hostTriple();
const universal =
  process.env.HARBOR_BUILD_UNIVERSAL_BRIDGE === "1" ||
  requested === "universal-apple-darwin";
const release =
  process.env.HARBOR_BRIDGE_PROFILE === "release" ||
  process.env.TAURI_ENV_DEBUG === "false" ||
  universal;

if (universal) {
  if (process.platform !== "darwin") {
    throw new Error("a universal Apple bridge must be prepared on macOS");
  }
  const arm = buildTarget("aarch64-apple-darwin", true);
  const intel = buildTarget("x86_64-apple-darwin", true);
  const combined = join(outputDir, `${binaryBase}-universal-apple-darwin`);
  rmSync(combined, { force: true });
  run("/usr/bin/lipo", ["-create", arm, intel, "-output", combined]);
  run("/usr/bin/lipo", [combined, "-verify_arch", "arm64", "x86_64"]);
  chmodSync(combined, 0o755);
} else {
  buildTarget(requested, release);
}
