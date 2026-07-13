import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";

/**
 * Wire the native OS drag-drop on this webview. Fires `onFolder(paths[0])` on a
 * drop, and returns a `dragging` flag for a drop-target overlay. Scope it to the
 * main window by mounting only there (the tray webview never calls this).
 */
export function useFolderDrop(onFolder: (path: string) => void) {
  const [dragging, setDragging] = useState(false);
  const cb = useRef(onFolder);
  cb.current = onFolder;

  useEffect(() => {
    const tauriReady = Boolean(
      (
        window as typeof window & {
          __TAURI_INTERNALS__?: { metadata?: unknown };
        }
      ).__TAURI_INTERNALS__?.metadata,
    );
    if (!tauriReady) return;

    let unlisten: (() => void) | undefined;
    let active = true;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        const p = e.payload;
        if (p.type === "enter" || p.type === "over") setDragging(true);
        else if (p.type === "leave") setDragging(false);
        else if (p.type === "drop") {
          setDragging(false);
          const first = p.paths?.[0]; // multi-drop: take the first, ignore rest
          if (first) cb.current(first);
        }
      })
      .then((u) => (active ? (unlisten = u) : u()));
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  return dragging;
}
