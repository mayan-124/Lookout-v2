// The add-ons offered for one-click "Turn on": the setup's "Daily features" step
// and the offer under a Clipboard or Capture search whose add-on is off. No
// imports and no DOM, so node can test it as it is.

export const CLIPBOARD_ID = "com.lookout.clipboard";
export const SCREENSHOT_ID = "dev.lookout.screenshot";

export const CANNOT_TURN_ON = "Could not turn on. Try again, or use Settings > Advanced > Add-ons.";

export interface Offer {
  id: string;
  name: string;
  what: string;
  privacy: string;
}

/** In the order the setup lists them. All are ticked on a first run. */
export const DAILY: readonly Offer[] = [
  {
    id: SCREENSHOT_ID,
    name: "Screenshots",
    what: "Capture or record your screen.",
    privacy: "Saved on this PC. Captures only when you ask. The frozen screen picture stays in memory until the capture box closes; it is written to disk only if you save.",
  },
  {
    id: "com.lookout.files-tools",
    name: "Rename and shrink files",
    what: "Rename many files, compress photos, remove photo details.",
    privacy: "Touches only files you pick, after you confirm.",
  },
  {
    id: "com.lookout.office-text",
    name: "Search Office files and PDFs",
    what: "Find words inside documents.",
    privacy: "Read on this PC. Excluded folders are never read.",
  },
  {
    id: CLIPBOARD_ID,
    name: "Clipboard",
    what: "Find text you copied before.",
    privacy: "Kept encrypted on this PC for 7 days. Secrets are masked when detected, which can miss some.",
  },
];

export const IMAGES = {
  name: "Keep copied pictures too",
  privacy: "The last 20, for 24 hours. Pictures are not checked for secrets.",
};

/** The add-on a Clipboard (section 2) or Capture (section 3) search needs. */
export const sectionOffer = (section: number): Offer | undefined =>
  DAILY.find((o) => o.id === (section === 2 ? CLIPBOARD_ID : section === 3 ? SCREENSHOT_ID : ""));

/** The ticks as the step opens: everything on a first run; when the setup is run again, only what is on. */
export const startTicks = (rerun: boolean, on: (id: string) => boolean): Record<string, boolean> =>
  Object.fromEntries(DAILY.map((o) => [o.id, !rerun || on(o.id)]));
