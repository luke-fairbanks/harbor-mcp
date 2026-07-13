// @vitest-environment jsdom

import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DownloadEvent } from "@tauri-apps/plugin-updater";

const native = vi.hoisted(() => ({
  check: vi.fn(),
  getVersion: vi.fn(),
  relaunch: vi.fn(),
}));

vi.mock("@tauri-apps/api/app", () => ({ getVersion: native.getVersion }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: native.relaunch }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: native.check }));

import { useAppUpdater } from "./useAppUpdater";

function fakeUpdate(
  downloadAndInstall: (
    onEvent?: (event: DownloadEvent) => void,
  ) => Promise<void> = async () => undefined,
) {
  return {
    body: "## A cleaner Harbor\n\nRelease details",
    close: vi.fn(async () => undefined),
    currentVersion: "0.4.0",
    downloadAndInstall: vi.fn(downloadAndInstall),
    rawJson: {},
    version: "0.5.0",
  };
}

describe("useAppUpdater", () => {
  beforeEach(() => {
    window.localStorage.clear();
    native.check.mockReset();
    native.getVersion.mockReset().mockResolvedValue("0.4.0");
    native.relaunch.mockReset().mockResolvedValue(undefined);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllEnvs();
  });

  it("reports a manual check when Harbor is current", async () => {
    native.check.mockResolvedValue(null);
    const { result } = renderHook(() => useAppUpdater());

    await waitFor(() =>
      expect(result.current.state.currentVersion).toBe("0.4.0"),
    );
    await act(() => result.current.checkForUpdates(true));

    expect(result.current.state.phase).toBe("upToDate");
    expect(result.current.state.source).toBe("manual");
    expect(result.current.state.checkedAt).not.toBeNull();
  });

  it("checks silently after startup in a production build", async () => {
    vi.useFakeTimers();
    vi.stubEnv("PROD", true);
    native.check.mockResolvedValue(null);
    const { result, unmount } = renderHook(() => useAppUpdater());

    await act(() => vi.advanceTimersByTimeAsync(2_500));

    expect(native.check).toHaveBeenCalledOnce();
    expect(result.current.state.phase).toBe("idle");
    expect(result.current.state.source).toBeNull();

    await act(() => vi.advanceTimersByTimeAsync(6 * 60 * 60 * 1_000));
    expect(native.check).toHaveBeenCalledTimes(2);
    unmount();
  });

  it("snoozes Later for the same version across a remount", async () => {
    const firstUpdate = fakeUpdate();
    native.check.mockResolvedValue(firstUpdate);
    const first = renderHook(() => useAppUpdater());

    await act(() => first.result.current.checkForUpdates(true));
    expect(first.result.current.state.phase).toBe("available");
    act(() => first.result.current.dismiss());
    expect(first.result.current.state.phase).toBe("idle");
    expect(window.localStorage.getItem("harbor.update-snooze")).toContain(
      "0.5.0",
    );
    first.unmount();

    const repeatedUpdate = fakeUpdate();
    native.check.mockResolvedValue(repeatedUpdate);
    const second = renderHook(() => useAppUpdater());
    await act(() => second.result.current.checkForUpdates(false));

    expect(second.result.current.state.phase).toBe("idle");
    expect(repeatedUpdate.close).toHaveBeenCalledOnce();
  });

  it("keeps an installed update restartable if relaunch fails", async () => {
    const update = fakeUpdate(async (onEvent) => {
      onEvent?.({ event: "Started", data: { contentLength: 100 } });
      onEvent?.({ event: "Progress", data: { chunkLength: 40 } });
      onEvent?.({ event: "Finished" });
    });
    native.check.mockResolvedValue(update);
    native.relaunch.mockRejectedValueOnce(new Error("restart IPC unavailable"));
    const { result } = renderHook(() => useAppUpdater());

    await act(() => result.current.checkForUpdates(true));
    await act(() => result.current.installUpdate());

    expect(update.downloadAndInstall).toHaveBeenCalledOnce();
    expect(result.current.state.downloadedBytes).toBe(40);
    expect(result.current.state.phase).toBe("restartRequired");
    expect(result.current.state.error).toMatch(/installed.*could not restart/i);

    await act(() => result.current.checkForUpdates(false));
    expect(native.check).toHaveBeenCalledOnce();
    expect(result.current.state.phase).toBe("restartRequired");

    native.relaunch.mockResolvedValue(undefined);
    await act(() => result.current.restartApp());
    expect(update.downloadAndInstall).toHaveBeenCalledOnce();
    expect(native.relaunch).toHaveBeenCalledTimes(2);
    expect(result.current.state.phase).toBe("relaunching");
  });

  it("turns native network errors into useful copy", async () => {
    native.check.mockRejectedValue(
      new Error("network request timed out with status: 503"),
    );
    const { result } = renderHook(() => useAppUpdater());

    await act(() => result.current.checkForUpdates(true));

    expect(result.current.state.phase).toBe("error");
    expect(result.current.state.error).toMatch(/check your connection/i);
    expect(result.current.state.errorDetails).toContain("status: 503");
  });

  it("surfaces signature rejection without relaunching", async () => {
    const update = fakeUpdate(async () => {
      throw new Error("signature verification failed");
    });
    native.check.mockResolvedValue(update);
    const { result } = renderHook(() => useAppUpdater());

    await act(() => result.current.checkForUpdates(true));
    await act(() => result.current.installUpdate());

    expect(result.current.state.phase).toBe("error");
    expect(result.current.state.error).toMatch(/security signature/i);
    expect(native.relaunch).not.toHaveBeenCalled();
  });

  it("releases an available native update when unmounted", async () => {
    const update = fakeUpdate();
    native.check.mockResolvedValue(update);
    const { result, unmount } = renderHook(() => useAppUpdater());

    await act(() => result.current.checkForUpdates(true));
    unmount();

    await waitFor(() => expect(update.close).toHaveBeenCalledOnce());
  });

  it("closes an update that arrives after the hook unmounts", async () => {
    let resolveCheck: (update: ReturnType<typeof fakeUpdate>) => void = () =>
      undefined;
    native.check.mockReturnValue(
      new Promise((resolve) => {
        resolveCheck = resolve;
      }),
    );
    const lateUpdate = fakeUpdate();
    const { result, unmount } = renderHook(() => useAppUpdater());
    let pendingCheck: Promise<void> | undefined;

    await act(async () => {
      pendingCheck = result.current.checkForUpdates(true);
      await Promise.resolve();
    });
    unmount();
    resolveCheck(lateUpdate);
    await pendingCheck;

    expect(lateUpdate.close).toHaveBeenCalledOnce();
  });
});
