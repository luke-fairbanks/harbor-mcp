#!/usr/bin/env node

/**
 * Exercise the exact stdio launcher installed for Claude Desktop and Codex.
 *
 * The test intentionally has no MCP SDK dependency. It speaks newline-delimited
 * JSON-RPC over stdio just like a desktop MCP host, validates the advertised
 * tool schemas against Claude Desktop's stricter expectations, and repeats
 * read-only calls long enough to cross the bridge's stream/reconnect window.
 *
 * Usage:
 *   node scripts/mcp-bridge-soak.mjs --duration-ms 75000 --interval-ms 30000
 *
 * Optional environment overrides:
 *   HARBOR_BRIDGE, HARBOR_SETTINGS, HARBOR_NPX
 */

import { spawn, spawnSync } from "node:child_process";
import { access, readFile } from "node:fs/promises";
import { constants as fsConstants } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import readline from "node:readline";

function numberArg(name, fallback) {
  const index = process.argv.indexOf(name);
  if (index === -1) return fallback;
  const value = Number(process.argv[index + 1]);
  if (!Number.isFinite(value) || value < 0) {
    throw new Error(`${name} must be a non-negative number`);
  }
  return value;
}

const durationMs = numberArg("--duration-ms", 75_000);
const intervalMs = Math.max(1, numberArg("--interval-ms", 30_000));
const requestTimeoutMs = Math.max(
  1,
  numberArg("--request-timeout-ms", 15_000),
);
const supportDir = join(
  homedir(),
  "Library",
  "Application Support",
  "com.harbor.desktop",
);
const bridge = process.env.HARBOR_BRIDGE ?? join(supportDir, "harbor-mcp-bridge");
const settings = process.env.HARBOR_SETTINGS ?? join(supportDir, "mcp.json");
const startedAt = Date.now();
const stamp = () => `${((Date.now() - startedAt) / 1_000).toFixed(3)}s`;

async function configuredNpx() {
  if (process.env.HARBOR_NPX) return process.env.HARBOR_NPX;

  const claudeConfig = join(
    homedir(),
    "Library",
    "Application Support",
    "Claude",
    "claude_desktop_config.json",
  );
  try {
    const config = JSON.parse(await readFile(claudeConfig, "utf8"));
    const configured = config?.mcpServers?.harbor?.env?.HARBOR_NPX;
    if (typeof configured === "string" && configured.length > 0) return configured;
  } catch {
    // Fall through to the login-shell lookup when Claude is not configured.
  }

  const lookup = spawnSync("/bin/zsh", ["-lc", "command -v npx"], {
    encoding: "utf8",
  });
  const detected = lookup.stdout.trim();
  if (lookup.status !== 0 || !detected) {
    throw new Error("could not resolve npx; set HARBOR_NPX to its absolute path");
  }
  return detected;
}

function findBooleanSchema(value, path = "schema") {
  if (typeof value === "boolean") return path;
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;

  // Only descend into positions whose values are themselves schemas. A boolean
  // used as data (for example `default: false`) is valid and must not be
  // mistaken for a boolean-form JSON Schema node.
  for (const key of [
    "additionalItems",
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
  ]) {
    if (key in value) {
      const found = findBooleanSchema(value[key], `${path}.${key}`);
      if (found) return found;
    }
  }

  for (const key of ["allOf", "anyOf", "oneOf", "prefixItems"]) {
    if (!Array.isArray(value[key])) continue;
    for (let index = 0; index < value[key].length; index += 1) {
      const found = findBooleanSchema(value[key][index], `${path}.${key}[${index}]`);
      if (found) return found;
    }
  }

  for (const key of [
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
    "properties",
  ]) {
    const entries = value[key];
    if (!entries || typeof entries !== "object" || Array.isArray(entries)) continue;
    for (const [name, schema] of Object.entries(entries)) {
      const found = findBooleanSchema(schema, `${path}.${key}.${name}`);
      if (found) return found;
    }
  }

  return undefined;
}

function assertClaudeCompatibleTools(tools) {
  if (!Array.isArray(tools) || tools.length === 0) {
    throw new Error("tools/list returned no tools");
  }
  for (const tool of tools) {
    if (!tool || typeof tool.name !== "string") {
      throw new Error("tools/list contained a tool without a name");
    }
    for (const [kind, schema] of [
      ["inputSchema", tool.inputSchema],
      ["outputSchema", tool.outputSchema],
    ]) {
      if (schema === undefined) continue;
      if (!schema || typeof schema !== "object" || Array.isArray(schema)) {
        throw new Error(`${tool.name}.${kind} must be a JSON Schema object`);
      }
      const booleanPath = findBooleanSchema(schema, `${tool.name}.${kind}`);
      if (booleanPath) {
        throw new Error(
          `${booleanPath} is a boolean JSON Schema; Claude Desktop rejects ` +
            "boolean schema nodes and discards Harbor's tool catalog",
        );
      }
    }
  }
}

async function main() {
  await access(bridge, fsConstants.X_OK);
  await access(settings, fsConstants.R_OK);
  const npx = await configuredNpx();
  await access(npx, fsConstants.X_OK);

  console.log(`[${stamp()}] launcher=${bridge}`);
  console.log(`[${stamp()}] settings=${settings}`);
  console.log(`[${stamp()}] npx=${npx}`);

  const child = spawn(bridge, [], {
    detached: true,
    env: {
      ...process.env,
      HARBOR_SETTINGS: settings,
      HARBOR_NPX: npx,
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const pending = new Map();
  let bridgeStderr = "";
  let nextId = 0;
  let exited;

  const exitPromise = new Promise((resolve) => {
    child.once("exit", (code, signal) => {
      exited = { code, signal };
      for (const request of pending.values()) {
        request.reject(new Error(`bridge exited (${code ?? signal})`));
      }
      pending.clear();
      resolve();
    });
  });

  child.stderr.on("data", (buffer) => {
    const message = buffer.toString();
    bridgeStderr += message;
    process.stderr.write(`[bridge ${stamp()}] ${message}`);
  });

  readline.createInterface({ input: child.stdout }).on("line", (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      process.stderr.write(`[bridge stdout ${stamp()}] ${line}\n`);
      return;
    }
    if (!("id" in message) || !pending.has(message.id)) return;
    const request = pending.get(message.id);
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result);
  });

  function request(method, params) {
    if (exited) return Promise.reject(new Error(`bridge exited (${exited.code})`));
    const id = nextId;
    nextId += 1;
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`${method} timed out after ${requestTimeoutMs}ms`));
      }, requestTimeoutMs);
      pending.set(id, { resolve, reject, timer });
    });
  }

  function notify(method, params = {}) {
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  async function readOnlyCheckpoint(label) {
    const catalog = await request("tools/list", {});
    assertClaudeCompatibleTools(catalog?.tools);
    if (!catalog.tools.some((tool) => tool.name === "list_apps")) {
      throw new Error(`${label} catalog does not contain list_apps`);
    }
    const call = await request("tools/call", {
      name: "list_apps",
      arguments: {},
    });
    if (call?.isError) throw new Error(`${label} list_apps returned isError=true`);
    console.log(
      `[${stamp()}] ${label}: list_apps ok; ${catalog.tools.length} Claude-compatible tools`,
    );
  }

  let failure;
  try {
    const initialized = await request("initialize", {
      protocolVersion: "2025-11-25",
      capabilities: {
        extensions: {
          "io.modelcontextprotocol/ui": {
            mimeTypes: ["text/html;profile=mcp-app"],
          },
        },
      },
      clientInfo: { name: "claude-ai", version: "0.1.0" },
    });
    console.log(`[${stamp()}] initialized protocol=${initialized.protocolVersion}`);
    notify("notifications/initialized");

    await readOnlyCheckpoint("immediate");
    const soakStartedAt = Date.now();
    let checkpoint = intervalMs;
    while (checkpoint < durationMs) {
      await new Promise((resolve) =>
        setTimeout(resolve, soakStartedAt + checkpoint - Date.now()),
      );
      await readOnlyCheckpoint(`+${checkpoint}ms`);
      checkpoint += intervalMs;
    }
    if (durationMs > 0) {
      await new Promise((resolve) =>
        setTimeout(resolve, soakStartedAt + durationMs - Date.now()),
      );
      await readOnlyCheckpoint(`+${durationMs}ms`);
    }
    const transportErrors = bridgeStderr.match(
      /SSE stream disconnected|Failed to (?:open|reconnect) SSE|Maximum reconnection attempts|HTTP 401/gi,
    );
    if (transportErrors) {
      throw new Error(`bridge emitted ${transportErrors.length} stream/auth/reconnect error(s)`);
    }
    console.log(`[${stamp()}] PASS`);
  } catch (error) {
    failure = error;
    console.error(`[${stamp()}] FAIL: ${error.message}`);
  } finally {
    child.stdin.end();
    if (!exited) {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // It may have exited between the check and the signal.
      }
      await Promise.race([
        exitPromise,
        new Promise((resolve) => setTimeout(resolve, 2_000)),
      ]);
    }
    if (!exited) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        // It may have exited between the check and the signal.
      }
    }
  }

  if (failure) throw failure;
}

main().catch((error) => {
  console.error(error.stack ?? error);
  process.exitCode = 1;
});
