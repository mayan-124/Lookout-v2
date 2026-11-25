// Reading a key combination from a capture box.

/**
 * What a key press in a capture box means: a combination, a hint for the user,
 * or nothing yet (Tab and Escape keep their usual job, modifiers wait for a key).
 */
export function captureCombo(e: KeyboardEvent): { combo: string } | { hint: string } | null {
  if (e.key === "Tab" || e.key === "Escape") return null;
  e.preventDefault();
  e.stopPropagation();
  if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return null;
  let key: string | null = null;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.code)) key = e.code;
  else if (e.code === "Space") key = "Space";
  if (!key) return { hint: "Use a letter, digit, F-key or Space with a modifier." };
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
  if (mods.length === 0) return { hint: "Hold Ctrl, Alt, Shift or Win together with the key." };
  return { combo: [...mods, key].join("+") };
}
