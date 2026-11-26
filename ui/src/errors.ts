// Backend errors as one plain sentence: no error codes, no internal wording.

const KNOWN: Record<string, string> = {
  "search is not running": "Search is not running. Restart Lookout.",
  "there is no finished scan": "There is no finished scan. Press Scan.",
  "there is no scan open": "There is no finished scan. Press Scan.",
  "that row is not in the scan": "That item is no longer in the scan. Scan again.",
  "nothing to preview": "There is nothing to preview. Select another result.",
};

// "(os error 5)", "(Windows error 5)", "(error 5)", "(0x80070005)". A bare
// number stays: file names such as "report (1).txt" carry one.
const CODE = /\s*\((?:(?:os error|windows error|error|hresult)\s*(?:0x[0-9a-f]+|-?\d+)|0x[0-9a-f]+)\)/gi;

export function plain(e: unknown): string {
  const raw = typeof e === "string" ? e : e instanceof Error ? e.message : "";
  // A script error is not a sentence for anyone.
  if (!raw || (e instanceof Error && e.name !== "Error")) return "Something went wrong. Try again.";
  const known = KNOWN[raw];
  if (known) return known;
  const text = raw
    .replace(CODE, "")
    .replace(/\bHRESULT\s*0x[0-9a-f]+/gi, "")
    .trim();
  if (!text) return "Something went wrong. Try again.";
  // Only a plain first word is capitalised, never a file name such as "node.exe".
  const first = /^[a-z]+(?=[\s,:]|$)/.test(text) ? text[0].toUpperCase() + text.slice(1) : text;
  return /[.!?…]$/.test(first) ? first : `${first}.`;
}
