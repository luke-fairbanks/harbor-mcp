import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";

const AUTO_CHECK_DELAY_MS = 2_500;
const AUTO_CHECK_INTERVAL_MS = 6 * 60 * 60 * 1_000;
const UPDATE_SNOOZE_MS = 24 * 60 * 60 * 1_000;
const UPDATE_SNOOZE_KEY = "harbor.update-snooze";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installing"
  | "relaunching"
  | "restartRequired"
  | "upToDate"
  | "error";

export interface AppUpdateState {
  phase: UpdatePhase;
  source: "automatic" | "manual" | null;
  currentVersion: string | null;
  nextVersion: string | null;
  notes: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  checkedAt: number | null;
  error: string | null;
  errorDetails: string | null;
}

export interface AppUpdaterController {
  state: AppUpdateState;
  checkForUpdates: (interactive?: boolean) => Promise<void>;
  installUpdate: () => Promise<void>;
  restartApp: () => Promise<void>;
  dismiss: () => void;
}

interface UpdateSnooze {
  version: string;
  until: number;
}

const initialState: AppUpdateState = {
  phase: "idle",
  source: null,
  currentVersion: null,
  nextVersion: null,
  notes: null,
  downloadedBytes: 0,
  totalBytes: null,
  checkedAt: null,
  error: null,
  errorDetails: null,
};

function technicalError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  return message.slice(0, 2_000);
}

function friendlyError(
  error: unknown,
  stage: "check" | "install" | "restart",
) {
  const details = technicalError(error);
  const normalized = details.toLowerCase();

  if (stage === "restart") {
    return {
      message: "The update is installed, but Harbor could not restart itself.",
      details,
    };
  }
  if (
    normalized.includes("signature") ||
    normalized.includes("verify") ||
    normalized.includes("verification") ||
    normalized.includes("minisign")
  ) {
    return {
      message:
        "Harbor rejected the download because its security signature could not be verified.",
      details,
    };
  }
  if (
    normalized.includes("network") ||
    normalized.includes("timed out") ||
    normalized.includes("timeout") ||
    normalized.includes("request") ||
    normalized.includes("connect") ||
    normalized.includes("dns") ||
    /status: [45]\d\d/.test(normalized)
  ) {
    return {
      message:
        "Harbor couldn’t reach the update service. Check your connection and try again.",
      details,
    };
  }
  if (
    normalized.includes("permission") ||
    normalized.includes("denied") ||
    normalized.includes("read-only") ||
    normalized.includes("install")
  ) {
    return {
      message:
        "Harbor couldn’t replace the installed app. Make sure Harbor is in Applications, then try again.",
      details,
    };
  }
  return {
    message:
      stage === "check"
        ? "Harbor couldn’t check for updates right now."
        : "Harbor couldn’t finish installing the update.",
    details,
  };
}

function readUpdateSnooze(): UpdateSnooze | null {
  try {
    const value = window.localStorage.getItem(UPDATE_SNOOZE_KEY);
    if (!value) return null;
    const parsed = JSON.parse(value) as Partial<UpdateSnooze>;
    if (
      typeof parsed.version !== "string" ||
      typeof parsed.until !== "number" ||
      parsed.until <= Date.now()
    ) {
      window.localStorage.removeItem(UPDATE_SNOOZE_KEY);
      return null;
    }
    return { version: parsed.version, until: parsed.until };
  } catch {
    return null;
  }
}

function saveUpdateSnooze(snooze: UpdateSnooze | null) {
  try {
    if (snooze) {
      window.localStorage.setItem(UPDATE_SNOOZE_KEY, JSON.stringify(snooze));
    } else {
      window.localStorage.removeItem(UPDATE_SNOOZE_KEY);
    }
  } catch {
    // A storage failure should never block an update or dismissal.
  }
}

export function useAppUpdater(): AppUpdaterController {
  const [state, setState] = useState<AppUpdateState>(initialState);
  const updateRef = useRef<Update | null>(null);
  const busyRef = useRef(false);
  const updateInstalledRef = useRef(false);
  const mountedRef = useRef(false);
  const operationGenerationRef = useRef(0);
  const snoozeRef = useRef<UpdateSnooze | null>(null);
  const snoozeInitializedRef = useRef(false);
  if (!snoozeInitializedRef.current) {
    snoozeRef.current = readUpdateSnooze();
    snoozeInitializedRef.current = true;
  }

  const closeCurrentUpdate = useCallback(async () => {
    const update = updateRef.current;
    updateRef.current = null;
    if (!update) return;
    try {
      await update.close();
    } catch {
      // The native resource may already have been released after installation.
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      operationGenerationRef.current += 1;
      const update = updateRef.current;
      updateRef.current = null;
      if (update) void update.close().catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((version) => {
        if (!cancelled) {
          setState((previous) => ({ ...previous, currentVersion: version }));
        }
      })
      .catch((error) => console.warn("Could not read Harbor version", error));
    return () => {
      cancelled = true;
    };
  }, []);

  const checkForUpdates = useCallback(
    async (interactive = true) => {
      if (
        busyRef.current ||
        updateInstalledRef.current ||
        (!interactive && updateRef.current)
      ) {
        return;
      }
      const operation = ++operationGenerationRef.current;
      busyRef.current = true;
      await closeCurrentUpdate();
      if (
        !mountedRef.current ||
        operation !== operationGenerationRef.current
      ) {
        busyRef.current = false;
        return;
      }
      setState((previous) => ({
        ...previous,
        phase: "checking",
        source: interactive ? "manual" : "automatic",
        nextVersion: null,
        notes: null,
        downloadedBytes: 0,
        totalBytes: null,
        error: null,
        errorDetails: null,
      }));

      try {
        const update = await check({ timeout: 15_000 });
        if (
          !mountedRef.current ||
          operation !== operationGenerationRef.current
        ) {
          if (update) await update.close().catch(() => undefined);
          return;
        }
        const checkedAt = Date.now();
        if (!update) {
          snoozeRef.current = null;
          saveUpdateSnooze(null);
          setState((previous) => ({
            ...previous,
            phase: interactive ? "upToDate" : "idle",
            source: interactive ? "manual" : null,
            checkedAt,
          }));
          return;
        }

        const snooze = snoozeRef.current;
        if (
          !interactive &&
          snooze?.version === update.version &&
          snooze.until > Date.now()
        ) {
          try {
            await update.close();
          } catch {
            // Best effort: dismissal should never turn into an error prompt.
          }
          if (
            !mountedRef.current ||
            operation !== operationGenerationRef.current
          ) {
            return;
          }
          setState((previous) => ({
            ...previous,
            phase: "idle",
            source: null,
            currentVersion: update.currentVersion,
            checkedAt,
          }));
          return;
        }

        updateRef.current = update;
        if (snooze?.version === update.version) {
          snoozeRef.current = null;
          saveUpdateSnooze(null);
        }
        setState((previous) => ({
          ...previous,
          phase: "available",
          source: interactive ? "manual" : "automatic",
          currentVersion: update.currentVersion,
          nextVersion: update.version,
          notes: update.body?.trim() || null,
          checkedAt,
        }));
      } catch (error) {
        if (
          !mountedRef.current ||
          operation !== operationGenerationRef.current
        ) {
          return;
        }
        if (interactive) {
          const described = friendlyError(error, "check");
          setState((previous) => ({
            ...previous,
            phase: "error",
            source: "manual",
            checkedAt: Date.now(),
            error: described.message,
            errorDetails: described.details,
          }));
        } else {
          console.warn("Harbor update check failed", error);
          setState((previous) => ({
            ...previous,
            phase: "idle",
            source: null,
            checkedAt: Date.now(),
          }));
        }
      } finally {
        busyRef.current = false;
      }
    },
    [closeCurrentUpdate],
  );

  const restartApp = useCallback(async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setState((previous) => ({
      ...previous,
      phase: "relaunching",
      error: null,
      errorDetails: null,
    }));
    try {
      await relaunch();
    } catch (error) {
      const described = friendlyError(error, "restart");
      setState((previous) => ({
        ...previous,
        phase: "restartRequired",
        error: described.message,
        errorDetails: described.details,
      }));
    } finally {
      busyRef.current = false;
    }
  }, []);

  const installUpdate = useCallback(async () => {
    const update = updateRef.current;
    if (!update || busyRef.current) return;
    busyRef.current = true;
    let downloadedBytes = 0;
    setState((previous) => ({
      ...previous,
      phase: "downloading",
      downloadedBytes: 0,
      totalBytes: null,
      error: null,
      errorDetails: null,
    }));

    let installed = false;
    try {
      await update.downloadAndInstall(
        (event) => {
          if (event.event === "Started") {
            setState((previous) => ({
              ...previous,
              totalBytes: event.data.contentLength ?? null,
            }));
          } else if (event.event === "Progress") {
            downloadedBytes += event.data.chunkLength;
            setState((previous) => ({
              ...previous,
              downloadedBytes,
            }));
          } else {
            setState((previous) => ({ ...previous, phase: "installing" }));
          }
        },
        { timeout: 5 * 60_000 },
      );
      installed = true;
      updateInstalledRef.current = true;
      await closeCurrentUpdate();
      snoozeRef.current = null;
      saveUpdateSnooze(null);
      setState((previous) => ({
        ...previous,
        phase: "restartRequired",
        error: null,
        errorDetails: null,
      }));
    } catch (error) {
      const described = friendlyError(error, "install");
      setState((previous) => ({
        ...previous,
        phase: "error",
        source: "manual",
        error: described.message,
        errorDetails: described.details,
      }));
    } finally {
      busyRef.current = false;
    }
    if (installed) await restartApp();
  }, [closeCurrentUpdate, restartApp]);

  const dismiss = useCallback(() => {
    if (
      state.phase === "downloading" ||
      state.phase === "installing" ||
      state.phase === "relaunching" ||
      state.phase === "restartRequired"
    ) {
      return;
    }
    if (state.phase === "available") {
      if (state.nextVersion) {
        const snooze = {
          version: state.nextVersion,
          until: Date.now() + UPDATE_SNOOZE_MS,
        };
        snoozeRef.current = snooze;
        saveUpdateSnooze(snooze);
      }
    }
    void closeCurrentUpdate();
    setState((previous) => ({
      ...previous,
      phase: "idle",
      source: null,
      nextVersion: null,
      notes: null,
      downloadedBytes: 0,
      totalBytes: null,
      error: null,
      errorDetails: null,
    }));
  }, [closeCurrentUpdate, state.nextVersion, state.phase]);

  useEffect(() => {
    // Development builds must never replace themselves with a public release.
    if (!import.meta.env.PROD) return;
    const firstCheck = window.setTimeout(
      () => void checkForUpdates(false),
      AUTO_CHECK_DELAY_MS,
    );
    const interval = window.setInterval(
      () => void checkForUpdates(false),
      AUTO_CHECK_INTERVAL_MS,
    );
    return () => {
      window.clearTimeout(firstCheck);
      window.clearInterval(interval);
    };
  }, [checkForUpdates]);

  return { state, checkForUpdates, installUpdate, restartApp, dismiss };
}
