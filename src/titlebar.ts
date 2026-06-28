import { getCurrentWindow } from "@tauri-apps/api/window";
import type { MouseEvent as ReactMouseEvent } from "react";

// Elements that should NOT start a window drag (they handle their own clicks),
// plus anything explicitly opted out with `data-no-drag` (e.g. selectable text).
const INTERACTIVE =
  'button, a, input, textarea, select, [role="combobox"], [role="menuitem"], [data-no-drag]';

/**
 * Start a native window drag from a title-bar region. Used instead of
 * `data-tauri-drag-region` (which didn't initiate drags here). Double-click
 * toggles maximize, matching macOS title-bar behavior. Interactive children are
 * left alone so buttons/menus keep working.
 */
export function startWindowDrag(e: ReactMouseEvent) {
  if (e.button !== 0) return;
  const target = e.target as HTMLElement | null;
  if (target && target.closest(INTERACTIVE)) return;
  const win = getCurrentWindow();
  if (e.detail === 2) {
    void win.toggleMaximize();
  } else {
    void win.startDragging();
  }
}
