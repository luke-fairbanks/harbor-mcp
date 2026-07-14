#!/usr/bin/env node

/**
 * Exercise the exact native stdio bridge installed for Claude Desktop, Claude
 * Code, and Codex.
 *
 * The test intentionally has no MCP SDK dependency. It speaks newline-delimited
 * JSON-RPC over stdio just like a desktop MCP host, validates the advertised
 * tool schemas against Claude Desktop's stricter expectations, and repeats
 * read-only calls long enough to prove the bridge stays healthy over time.
 *
 * Usage:
 *   node scripts/mcp-bridge-soak.mjs --duration-ms 75000 --interval-ms 30000
 *   node scripts/mcp-bridge-soak.mjs --restart-harbor
 *
 * Optional environment overrides:
 *   HARBOR_BRIDGE, HARBOR_SETTINGS
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
const restartHarbor = process.argv.includes("--restart-harbor");
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

const sleep = (duration) =>
  new Promise((resolve) => setTimeout(resolve, duration));

async function readDescriptorSnapshot() {
  let descriptor;
  try {
    descriptor = JSON.parse(await readFile(settings, "utf8"));
  } catch {
    throw new Error("could not read Harbor's endpoint descriptor");
  }
  if (
    !descriptor ||
    typeof descriptor !== "object" ||
    typeof descriptor.token !== "string" ||
    descriptor.token.length < 16
  ) {
    throw new Error("Harbor's endpoint descriptor is invalid");
  }
  return {
    instanceId:
      typeof descriptor.instanceId === "string" ? descriptor.instanceId : "",
    pid:
      Number.isSafeInteger(descriptor.pid) && descriptor.pid > 0
        ? descriptor.pid
        : 0,
    token: descriptor.token,
  };
}

function processIsRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code !== "ESRCH";
  }
}

async function waitForProcessExit(pid, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (processIsRunning(pid)) {
    if (Date.now() >= deadline) {
      throw new Error(`Harbor pid ${pid} did not exit after a clean quit request`);
    }
    await sleep(100);
  }
}

async function waitForDescriptorChange(previous, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let latest;
  while (Date.now() < deadline) {
    try {
      latest = await readDescriptorSnapshot();
      if (latest.instanceId && latest.instanceId !== previous.instanceId) {
        return latest;
      }
    } catch {
      // Harbor may be replacing the descriptor atomically while it starts.
    }
    await sleep(100);
  }
  throw new Error("Harbor restarted without publishing a new instance identity");
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
  const initialDescriptor = await readDescriptorSnapshot();
  if (
    !restartHarbor &&
    (initialDescriptor.pid <= 0 || !processIsRunning(initialDescriptor.pid))
  ) {
    throw new Error(
      "Start Harbor before the non-mutating soak; use --restart-harbor to test recovery",
    );
  }
  const protectedSecrets = new Set();
  const rememberDescriptor = (descriptor) => {
    protectedSecrets.add(descriptor.token);
    protectedSecrets.add(descriptor.token.slice(0, 12));
  };
  rememberDescriptor(initialDescriptor);

  console.log(`[${stamp()}] native bridge=${bridge}`);
  console.log(`[${stamp()}] settings=${settings}`);

  // Preserve the normal process environment, but do not pass legacy or test
  // HARBOR_* controls into the production bridge. Its only override is the
  // protected descriptor path used by the installed client configurations.
  const bridgeEnv = Object.fromEntries(
    Object.entries(process.env).filter(([name]) => !name.startsWith("HARBOR_")),
  );
  bridgeEnv.HARBOR_SETTINGS = settings;

  const child = spawn(bridge, [], {
    env: bridgeEnv,
    stdio: ["pipe", "pipe", "pipe"],
  });
  const bridgePid = child.pid;
  if (!Number.isSafeInteger(bridgePid) || bridgePid <= 0) {
    throw new Error("native bridge did not start with a valid process id");
  }
  console.log(`[${stamp()}] bridge pid=${bridgePid}`);

  const pending = new Map();
  let bridgeStdout = "";
  let bridgeStderr = "";
  let stdoutProtocolErrors = 0;
  let nextId = 0;
  let restartedHarborPid = 0;
  let exited;
  let closed = false;

  const exitPromise = new Promise((resolve) => {
    child.once("exit", (code, signal) => {
      exited = { code, signal };
      for (const request of pending.values()) {
        clearTimeout(request.timer);
        request.reject(new Error(`bridge exited (${code ?? signal})`));
      }
      pending.clear();
      resolve();
    });
  });
  const closePromise = new Promise((resolve) => {
    child.once("close", () => {
      closed = true;
      resolve();
    });
  });

  child.stdout.on("data", (buffer) => {
    bridgeStdout += buffer.toString();
  });

  child.stderr.on("data", (buffer) => {
    // Capture, but never echo, bridge stderr. The assertions below prove it is
    // sanitized before the harness reports even aggregate information about it.
    bridgeStderr += buffer.toString();
  });

  readline.createInterface({ input: child.stdout }).on("line", (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      stdoutProtocolErrors += 1;
      return;
    }
    if (!message || typeof message !== "object" || message.jsonrpc !== "2.0") {
      stdoutProtocolErrors += 1;
      return;
    }
    if (!("id" in message) || !pending.has(message.id)) return;
    const request = pending.get(message.id);
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) {
      const code = Number.isSafeInteger(message.error.code)
        ? message.error.code
        : "unknown";
      request.reject(new Error(`${request.method} failed with JSON-RPC code ${code}`));
    } else request.resolve(message.result);
  });

  function assertBridgePid() {
    if (child.pid !== bridgePid || exited || !processIsRunning(bridgePid)) {
      throw new Error(`native bridge pid ${bridgePid} did not remain running`);
    }
  }

  function assertNoCredentialLeaks() {
    for (const [stream, output] of [
      ["stdout", bridgeStdout],
      ["stderr", bridgeStderr],
    ]) {
      for (const secret of protectedSecrets) {
        if (secret && output.includes(secret)) {
          throw new Error(`native bridge exposed descriptor credentials on ${stream}`);
        }
      }
    }
  }

  function assertNativeTransportOutput() {
    if (stdoutProtocolErrors > 0) {
      throw new Error(
        `native bridge emitted ${stdoutProtocolErrors} non-JSON-RPC stdout line(s)`,
      );
    }
    const transportErrors = bridgeStderr.match(
      /Harbor MCP bridge (?:could not initialize securely|lost its input stream)\.?/gi,
    );
    if (transportErrors) {
      throw new Error(
        `native bridge emitted ${transportErrors.length} transport error(s)`,
      );
    }
  }

  function request(method, params, timeoutMs = requestTimeoutMs) {
    if (exited) return Promise.reject(new Error(`bridge exited (${exited.code})`));
    const id = nextId;
    nextId += 1;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`${method} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      pending.set(id, { method, resolve, reject, timer });
      child.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`,
        (error) => {
          if (!error || !pending.has(id)) return;
          pending.delete(id);
          clearTimeout(timer);
          reject(new Error(`could not write ${method} to the native bridge`));
        },
      );
    });
  }

  function notify(method, params = {}) {
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  async function readOnlyCheckpoint(label, timeoutMs = requestTimeoutMs) {
    assertBridgePid();
    rememberDescriptor(await readDescriptorSnapshot());
    assertNoCredentialLeaks();
    const catalog = await request("tools/list", {}, timeoutMs);
    assertClaudeCompatibleTools(catalog?.tools);
    if (!catalog.tools.some((tool) => tool.name === "list_apps")) {
      throw new Error(`${label} catalog does not contain list_apps`);
    }
    rememberDescriptor(await readDescriptorSnapshot());
    assertNoCredentialLeaks();
    const call = await request(
      "tools/call",
      {
        name: "list_apps",
        arguments: {},
      },
      timeoutMs,
    );
    if (call?.isError) throw new Error(`${label} list_apps returned isError=true`);
    rememberDescriptor(await readDescriptorSnapshot());
    assertNoCredentialLeaks();
    assertBridgePid();
    console.log(
      `[${stamp()}] ${label}: list_apps ok; ${catalog.tools.length} Claude-compatible tools`,
    );
  }

  async function restartCheckpoint() {
    if (process.platform !== "darwin") {
      throw new Error("--restart-harbor is supported only on macOS");
    }

    const previous = await readDescriptorSnapshot();
    rememberDescriptor(previous);
    if (!previous.instanceId || previous.pid <= 0) {
      throw new Error(
        "--restart-harbor requires Harbor's reconnecting endpoint descriptor",
      );
    }
    if (!processIsRunning(previous.pid)) {
      throw new Error(`Harbor descriptor pid ${previous.pid} is not running`);
    }
    assertNoCredentialLeaks();
    assertBridgePid();

    const quit = spawnSync(
      "/usr/bin/osascript",
      ["-e", 'tell application id "com.harbor.desktop" to quit'],
      { encoding: "utf8", shell: false, timeout: 10_000 },
    );
    if (quit.error || quit.status !== 0) {
      throw new Error("macOS could not request a clean Harbor quit");
    }
    console.log(`[${stamp()}] clean quit requested for Harbor pid=${previous.pid}`);
    await waitForProcessExit(previous.pid, 15_000);
    assertBridgePid();

    const reconnectTimeoutMs = Math.max(requestTimeoutMs, 30_000);
    await readOnlyCheckpoint("after Harbor restart", reconnectTimeoutMs);
    const current = await waitForDescriptorChange(previous, reconnectTimeoutMs);
    rememberDescriptor(current);
    if (current.instanceId === previous.instanceId) {
      throw new Error("Harbor instance identity did not change after restart");
    }
    if (current.pid <= 0 || !processIsRunning(current.pid)) {
      throw new Error("Harbor published a new instance that is not running");
    }
    restartedHarborPid = current.pid;
    assertNoCredentialLeaks();
    assertBridgePid();
    console.log(
      `[${stamp()}] reconnect ok; Harbor pid=${current.pid}; bridge pid=${bridgePid} unchanged`,
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
    if (restartHarbor) await restartCheckpoint();
    const soakStartedAt = Date.now();
    let checkpoint = intervalMs;
    while (checkpoint < durationMs) {
      await sleep(Math.max(0, soakStartedAt + checkpoint - Date.now()));
      await readOnlyCheckpoint(`+${checkpoint}ms`);
      checkpoint += intervalMs;
    }
    if (durationMs > 0) {
      await sleep(Math.max(0, soakStartedAt + durationMs - Date.now()));
      await readOnlyCheckpoint(`+${durationMs}ms`);
    }
  } catch (error) {
    failure = error;
  } finally {
    child.stdin.end();
    if (!exited) {
      await Promise.race([
        exitPromise,
        new Promise((resolve) => setTimeout(resolve, 2_000)),
      ]);
    }
    if (!exited) {
      try {
        child.kill("SIGTERM");
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
        child.kill("SIGKILL");
      } catch {
        // It may have exited between the check and the signal.
      }
      await Promise.race([
        exitPromise,
        new Promise((resolve) => setTimeout(resolve, 2_000)),
      ]);
    }
    if (!closed) {
      await Promise.race([
        closePromise,
        new Promise((resolve) => setTimeout(resolve, 2_000)),
      ]);
    }
    if (!failure && (!exited || exited.code !== 0)) {
      failure = new Error("native bridge did not exit cleanly after stdin EOF");
    }
    if (!failure && !closed) {
      failure = new Error("native bridge output streams did not close cleanly");
    }
    if (
      restartHarbor &&
      restartedHarborPid > 0 &&
      !processIsRunning(restartedHarborPid)
    ) {
      failure = new Error("restarted Harbor exited when the bridge input closed");
    }

    // A restart can rotate the token just before a failure. Remember the
    // latest value, then inspect all buffered child output one final time.
    try {
      rememberDescriptor(await readDescriptorSnapshot());
    } catch {
      if (!failure) {
        failure = new Error("could not verify Harbor's final endpoint descriptor");
      }
    }
    try {
      assertNoCredentialLeaks();
    } catch (error) {
      failure = error;
    }
    if (!failure) {
      try {
        assertNativeTransportOutput();
      } catch (error) {
        failure = error;
      }
    }
  }

  if (failure) throw failure;
  if (bridgeStderr.length > 0) {
    console.log(
      `[${stamp()}] bridge stderr sanitized (${Buffer.byteLength(bridgeStderr)} bytes)`,
    );
  }
  console.log(`[${stamp()}] PASS; bridge pid=${bridgePid} remained constant`);
}

main().catch((error) => {
  console.error(error.stack ?? error);
  process.exitCode = 1;
});
