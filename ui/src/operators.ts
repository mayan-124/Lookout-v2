// What the search box offers while `ext:`, `size:` or `in:` is typed, and how a
// chip takes its operator out again. Pure functions; the backend does the parsing.

/** Values already in memory, from the `operator_values` command. */
export interface OperatorValues {
  extensions: string[];
  folders: string[];
}

export const NO_VALUES: OperatorValues = { extensions: [], folders: [] };

export const SIZE_EXAMPLES = [">1mb", ">10mb", ">100mb", ">1gb", "<1mb", "10mb..50mb"];
export const OPERATOR_HINT = "Try ext:pdf or size:>10mb";
const MAX_SHOWN = 8;

export interface Completion {
  /** What the list shows: "ext:pdf". */
  label: string;
  /** The whole box after accepting it. */
  text: string;
}

/** The operator being typed at the end of the box: its name and value so far. */
const TAIL = /(^|\s)(ext|size|in):("[^"]*|[^\s"]*)$/i;

export function completions(query: string, values: OperatorValues): Completion[] {
  const m = TAIL.exec(query);
  if (!m) return [];
  const key = m[2].toLowerCase();
  let typed = m[3];
  // The operator with whatever stays in front of the completed part.
  let head = `${m[2]}:`;
  let options = key === "ext" ? values.extensions : key === "size" ? SIZE_EXAMPLES : values.folders;
  if (key === "ext") {
    // After a comma only the last extension is being typed.
    const done = typed.slice(0, typed.lastIndexOf(",") + 1);
    typed = typed.slice(done.length);
    head += done;
    const used = done.toLowerCase().split(",");
    options = options.filter((o) => !used.includes(o));
  }
  const want = typed.replace(/^"/, "").toLowerCase();
  // Typed in full: nothing left to offer.
  if (options.some((o) => o.toLowerCase() === want)) return [];
  const before = query.slice(0, m.index + m[1].length);
  return options
    .filter((o) => o.toLowerCase().startsWith(want))
    .slice(0, MAX_SHOWN)
    .map((o) => {
      const value = /\s/.test(o) || typed.startsWith('"') ? `"${o}"` : o;
      return { label: `${key}:${value}`, text: `${before}${head}${value}` };
    });
}

/** The faint rest of the first completion, when it simply continues the text. */
export function operatorGhost(query: string, found: Completion[]): string {
  const first = found[0]?.text;
  return first?.toLowerCase().startsWith(query.toLowerCase()) ? first.slice(query.length) : "";
}

/** The box without one operator, exactly as the chip shows it. */
export function removeOperator(query: string, typed: string): string {
  for (let at = query.indexOf(typed); at >= 0; at = query.indexOf(typed, at + 1)) {
    const end = at + typed.length;
    // A whole token only: not the "ext:p" inside "ext:pdf".
    if ((at > 0 && !/\s/.test(query[at - 1])) || (end < query.length && !/\s/.test(query[end]))) continue;
    const left = query.slice(0, at).trimEnd();
    const right = query.slice(end).trimStart();
    return left && right ? `${left} ${right}` : left || right;
  }
  return query;
}
