// The one line that says how indexing stands, built only from real counters.

export interface IndexCounts {
  files: number;
  roots: number;
  /** File names are being listed; how many there will be is not known. */
  indexing: boolean;
  /** Paused by the user (pause key or tray). */
  paused: boolean;
  read_done: number;
  read_total: number;
  ocr_done: number;
  ocr_total: number;
  /** File reading holds back because the user is active. */
  waiting_for_idle: boolean;
  /** File reading will hold back at the next input. */
  pauses_on_activity: boolean;
}

export type IndexState = "loading" | "nofolders" | "paused" | "waiting" | "running" | "done";

export function indexLine(s: IndexCounts | null): { state: IndexState; text: string } {
  if (!s) return { state: "loading", text: "Checking files…" };
  const n = (v: number) => v.toLocaleString("en-US");
  const files = (v: number) => `${n(v)} ${v === 1 ? "file" : "files"}`;
  if (s.roots === 0 && s.files === 0) return { state: "nofolders", text: "No folders chosen" };
  if (s.paused) return { state: "paused", text: `Indexing paused · ${files(s.files)} ready` };
  if (s.indexing) return { state: "running", text: `Indexing ${files(s.files)}` };
  const total = s.read_total + s.ocr_total;
  const done = Math.min(total, s.read_done + s.ocr_done);
  if (s.waiting_for_idle) {
    const progress = total > 0 ? `${n(done)} of ${files(total)} read` : `${files(s.files)} ready`;
    return { state: "waiting", text: `Paused while you use the PC · ${progress}` };
  }
  if (done < total) {
    const pauses = s.pauses_on_activity ? " · pauses when you use the PC" : "";
    return { state: "running", text: `Indexing ${n(done)} of ${files(total)}${pauses}` };
  }
  return { state: "done", text: `${files(s.files)} ready` };
}

/**
 * The status part of the footer line: only while something is happening. How
 * many files are ready is said on Home alone.
 */
export function footerStatus(s: IndexCounts | null): string {
  const { state, text } = indexLine(s);
  if (state === "paused") return "Indexing paused";
  if (state !== "running" && state !== "waiting") return "";
  return text.replace(/ · [\d,]+ files? ready$/, "");
}
