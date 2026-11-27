// Real icons for app rows. Only rows on screen are asked for, one at a time;
// until an icon arrives, or if there is none, the row keeps the generic one.

/** Icons kept by the page, the same number the backend keeps. */
export const KEPT = 200;

const STORE = "shell:AppsFolder\\";
const PROGRAM = /^[a-z]:[\\/].*\.exe$/i;

/** What to ask an icon for: an app, or a program found as a file. */
export function iconTarget(kind: string, uri: string | null | undefined): string | null {
  if (!uri) return null;
  if (kind === "app") return uri.startsWith(STORE) || /\.(lnk|exe)$/i.test(uri) ? uri : null;
  return kind === "file" && PROGRAM.test(uri) ? uri : null;
}

export function iconLoader(
  fetch: (path: string) => Promise<string | null>,
  /** An icon to show, or `null` to forget it. */
  put: (path: string, src: string | null) => void,
) {
  const shown = new Map<string, number>(); // rows on screen per path, oldest first
  const done = new Map<string, string | null>();
  let active = true;
  let busy = false;
  let round = 0;

  async function pump() {
    if (busy) return;
    busy = true;
    while (active) {
      const path = [...shown.keys()].find((p) => !done.has(p));
      if (path === undefined) break;
      const asked = round;
      const src = await fetch(path).catch(() => null);
      if (asked !== round) continue;
      done.set(path, src);
      if (src) put(path, src);
      for (const old of done.keys()) {
        if (done.size <= KEPT) break;
        if (shown.has(old)) continue;
        done.delete(old);
        put(old, null);
      }
    }
    busy = false;
  }

  return {
    show(path: string) {
      shown.set(path, (shown.get(path) ?? 0) + 1);
      void pump();
    },
    hide(path: string) {
      const n = shown.get(path) ?? 0;
      if (n > 1) shown.set(path, n - 1);
      else shown.delete(path);
    },
    /** Off while the bar is hidden: nothing is asked for and nothing is kept. */
    setActive(on: boolean) {
      active = on;
      if (on) return void pump();
      round++;
      for (const path of done.keys()) put(path, null);
      done.clear();
    },
  };
}

type Watch = { observe(node: Element): void; unobserve(node: Element): void };
type Seen = { target: Element; isIntersecting: boolean }[];

/** A Svelte action: tells the loader when the row's icon is on screen. */
export function rowWatcher(
  loader: { show(path: string): void; hide(path: string): void },
  observer: (changed: (entries: Seen) => void) => Watch,
) {
  const rows = new Map<Element, { path: string; on: boolean }>();
  let watch: Watch | null = null;
  return (node: Element, path: string) => {
    watch ??= observer((entries) => {
      for (const e of entries) {
        const row = rows.get(e.target);
        if (!row || row.on === e.isIntersecting) continue;
        row.on = e.isIntersecting;
        if (row.on) loader.show(row.path);
        else loader.hide(row.path);
      }
    });
    const row = { path, on: false };
    rows.set(node, row);
    watch.observe(node);
    return {
      update(next: string) {
        if (next === row.path) return;
        if (row.on) {
          loader.hide(row.path);
          loader.show(next);
        }
        row.path = next;
      },
      destroy() {
        if (row.on) loader.hide(row.path);
        rows.delete(node);
        watch?.unobserve(node);
      },
    };
  };
}
