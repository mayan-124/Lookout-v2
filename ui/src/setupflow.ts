// When the first-run setup is open, and on which step.

export const STEPS = ["Welcome", "Folders", "Shortcuts", "Theme", "Try it"] as const;
/** Step numbers start at 1; "Choose folders to search" opens this one. */
export const FOLDER_STEP = 2;

/**
 * The step to show when the bar appears; null means no setup.
 *
 * `open` is the step already open, `needsSetup` is true until the setup was
 * finished or skipped (and never for an install that has folders), and
 * `restore` is true when the bar comes back as it was left.
 */
export function stepOnShow(open: number | null, needsSetup: boolean, restore: boolean): number | null {
  if (needsSetup) return open ?? 1;
  return restore ? open : null;
}

/** "Choose folders to search": the setup while it is pending, else Settings > Folders. */
export function chooseFoldersTarget(needsSetup: boolean): "setup" | "settings" {
  return needsSetup ? "setup" : "settings";
}

/** Text typed in the bar that opens the help view. */
export const isHelpQuery = (text: string) => /^\s*(help|\?)\s*$/i.test(text);

/** The examples of the last step. */
export const TRY_EXAMPLES: [string, string][] = [
  ["report", "Find a file by name"],
  ["timer 5 min", "Start a timer"],
  ["pdf last week", "Find files by date"],
];
