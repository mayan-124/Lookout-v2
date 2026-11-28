// The interface choices, shared by the bar, Settings and the setup. They are
// saved in the settings file and only change what is shown.
import { invoke } from "@tauri-apps/api/core";
import { resetLook } from "./look.svelte";
import type { Feature, Mode, UiState } from "./modes";
import { plain } from "./errors";

export const ui = $state<UiState>({
  mode: "standard",
  custom: false,
  show: { disk: false, clipboard: true, capture: true, timers: true, quick_keys: true, filters: true, notes: true },
  hide_tips: false,
  advanced_open: false,
  hide_after_copy: true,
  compact_home: false,
});

/** Runs a command that answers with the new state. Returns the error, or "". */
async function apply(command: string, args?: Record<string, unknown>): Promise<string> {
  try {
    Object.assign(ui, await invoke<UiState>(command, args));
    return "";
  } catch (e) {
    return plain(e);
  }
}

export const loadUi = () => apply("ui_state");
export const setMode = (mode: Mode) => apply("set_ui_mode", { mode });
export const setShow = (feature: Feature, on: boolean) => apply("set_ui_show", { show: { ...ui.show, [feature]: on } });
export const setHideTips = (hide: boolean) => apply("set_hide_tips", { hide });
export const setHideAfterCopy = (hide: boolean) => apply("set_hide_after_copy", { hide });
export const setCompactHome = (compact: boolean) => apply("set_compact_home", { compact });
export const setAdvancedOpen = (open: boolean) => apply("set_advanced_open", { open });

/** Visibility, theme and layout back to recommended. Data is not touched. */
export async function resetToRecommended(): Promise<string> {
  const error = await apply("reset_ui");
  if (!error) resetLook();
  return error;
}
