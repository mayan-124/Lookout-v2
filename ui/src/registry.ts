// The one list of what Lookout can do. Help, the Home tips, the quick-actions
// row, suggestions and ghost text all read it. To add a feature, add an entry to
// registry.json; core/tests/registry.rs checks its example is understood.
import data from "./registry.json" with { type: "json" };
import { advertised, type Action, type Mode, type Show } from "./modes.ts";

export const ACTIONS = data as Action[];

export const actionById = (id: string) => ACTIONS.find((a) => a.id === id);

/** The quick-actions row on Home, in its own order. */
export const homeActions = (mode: Mode, show: Show) =>
  ACTIONS.filter((a) => a.home && advertised(a, mode, show)).sort((a, b) => a.home! - b.home!);

/** One simple 16x16 line icon per category. */
export const ICONS: Record<string, string> = {
  Search: "M11.5 7a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0zM10.5 10.5L14 14",
  "Time filters": "M2.5 3.5h11V14h-11zM2.5 7h11M5.5 2v3M10.5 2v3",
  Timers: "M13 9a5 5 0 1 1-10 0 5 5 0 0 1 10 0zM8 9V6.4M6.5 2h3",
  "Quick keys": "M7.9 10.8a2.7 2.7 0 1 1-5.4 0 2.7 2.7 0 0 1 5.4 0zM7.2 8.8l6.3-6.3M11 5l1.8 1.8",
  Settings: "M2 4.5h7M12 4.5h2M2 11.5h2M7 11.5h7M12 4.5a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0zM7 11.5a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0z",
  System: "M10 8a2 2 0 1 1-4 0 2 2 0 0 1 4 0zM8 2v2M8 12v2M2 8h2M12 8h2M3.8 3.8l1.4 1.4M10.8 10.8l1.4 1.4M3.8 12.2l1.4-1.4M10.8 5.2l1.4-1.4",
  Disk: "M13.5 8a5.5 5.5 0 1 1-11 0 5.5 5.5 0 0 1 11 0zM8 8V2.5M8 8l4 3",
  Clipboard: "M3.5 3h9v10.5h-9zM6 3V2h4v1M5.8 7h4.4M5.8 9.5h4.4",
  Capture: "M2 5.5v-2A1.5 1.5 0 0 1 3.5 2h2M10 2h2.5A1.5 1.5 0 0 1 14 3.5v2M14 10.5v2a1.5 1.5 0 0 1-1.5 1.5H10M6 14H3.5A1.5 1.5 0 0 1 2 12.5v-2",
  "Command line": "M3 4l4 4-4 4M8.5 12.5H13",
  Help: "M14 8A6 6 0 1 1 2 8a6 6 0 0 1 12 0zM6.2 6.2a1.9 1.9 0 1 1 2.6 1.8c-.6.3-.8.7-.8 1.4M8 11.6v.1",
};
