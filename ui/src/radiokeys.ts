/** Arrow keys move and pick within a radio group. */
export function radioKeys(e: KeyboardEvent) {
  const step = ["ArrowRight", "ArrowDown"].includes(e.key) ? 1 : ["ArrowLeft", "ArrowUp"].includes(e.key) ? -1 : 0;
  const from = e.target as HTMLElement;
  if (!step || from.getAttribute("role") !== "radio") return;
  e.preventDefault();
  const all = [...from.closest('[role="radiogroup"]')!.querySelectorAll<HTMLButtonElement>('[role="radio"]')];
  const next = all[(all.indexOf(from as HTMLButtonElement) + step + all.length) % all.length];
  next.focus();
  next.click();
}
