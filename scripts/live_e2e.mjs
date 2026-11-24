// Drives the real release lookout.exe through the WebView2 debugging port.
// Victim: examples/test_listener.exe (listens on a loopback port, nothing else).
import { spawn, execFileSync } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";

import { fileURLToPath } from "node:url";
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const APP = path.join(REPO, "target", "release", "lookout.exe");
const LISTENER = path.join(REPO, "target", "debug", "examples", "test_listener.exe");
const CDP = 9333;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = "") => {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${extra ? "  -> " + extra : ""}`);
};

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "lookout-live-"));
const dataDir = path.join(tmp, "data");
const root = path.join(tmp, "files");
const work = path.join(tmp, "work");
const marker = path.join(tmp, "bat-ran.txt");
for (const d of [dataDir, root, work]) fs.mkdirSync(d, { recursive: true });
fs.writeFileSync(path.join(root, "live alpha.txt"), "hello");
fs.writeFileSync(path.join(root, "live tool alpha.bat"), `@echo ran>"${marker}"\r\n`);

const freePort = () =>
  new Promise((res) => {
    const s = net.createServer().listen(0, "127.0.0.1", () => {
      const p = s.address().port;
      s.close(() => res(p));
    });
  });
const isListening = (port) =>
  new Promise((res) => {
    const s = net.connect(port, "127.0.0.1");
    s.on("connect", () => (s.destroy(), res(true)));
    s.on("error", () => res(false));
  });
const until = async (what, fn, ms = 15000) => {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("timeout: " + what);
    await sleep(50);
  }
};

let app = null;
const children = [];
function launchApp() {
  app = spawn(APP, ["--data-dir", dataDir, "--index-root", root], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${CDP}` },
    stdio: "ignore",
  });
}
function stopApp() {
  if (!app) return;
  try {
    execFileSync("taskkill", ["/T", "/F", "/PID", String(app.pid)], { stdio: "ignore" });
  } catch {}
  app = null;
}

const logErrors = []; // console errors, including CSP violations
let ws, nextId = 1;
const pending = new Map();
async function connect() {
  const target = await until("cdp target", async () => {
    try {
      const list = await (await fetch(`http://127.0.0.1:${CDP}/json`)).json();
      return list.find((t) => t.type === "page" && /tauri|localhost/.test(t.url));
    } catch {
      return null;
    }
  });
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.method === "Log.entryAdded" && d.params.entry.level === "error") logErrors.push(d.params.entry.text);
    if (d.method === "Runtime.exceptionThrown") logErrors.push(JSON.stringify(d.params.exceptionDetails.text));
    if (d.id && pending.has(d.id)) pending.get(d.id)(d), pending.delete(d.id);
  };
}
function cdp(method, params = {}) {
  return new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
// Real (trusted) input through the browser's input pipeline, not DOM-made events.
async function realEnter() {
  const k = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 };
  await cdp("Input.dispatchKeyEvent", { type: "keyDown", text: String.fromCharCode(13), ...k });
  await cdp("Input.dispatchKeyEvent", { type: "keyUp", ...k });
}
async function realClick(label) {
  const r = await ev(`(() => { const b = [...document.querySelectorAll('.card .btn')].find(b => b.textContent.trim().startsWith(${JSON.stringify(label)})); const r = b.getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()`);
  const m = { x: r.x, y: r.y, button: "left", clickCount: 1 };
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: r.x, y: r.y });
  await cdp("Input.dispatchMouseEvent", { type: "mousePressed", ...m });
  await cdp("Input.dispatchMouseEvent", { type: "mouseReleased", ...m });
}
const syntheticClick = (label) =>
  ev(`[...document.querySelectorAll('.card .btn')].find(b => b.textContent.trim().startsWith(${JSON.stringify(label)})).click()`);

function ev(expression) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, (d) => (d.result?.exceptionDetails ? reject(new Error(JSON.stringify(d.result.exceptionDetails.exception?.description ?? d.result.exceptionDetails))) : resolve(d.result?.result?.value)));
    ws.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression, awaitPromise: true, returnByValue: true } }));
  });
}
const invoke = (cmd, args = {}) =>
  ev(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)}).then(v => ({ok:true, v}), e => ({ok:false, e:String(e)}))`);
const typeQuery = (text) =>
  ev(`(() => { const i = document.querySelector('input.search'); i.focus(); i.value = ${JSON.stringify(text)}; i.dispatchEvent(new Event('input', {bubbles:true})); })()`);
const key = (k, extra = {}) =>
  ev(`window.dispatchEvent(new KeyboardEvent('keydown', Object.assign({key:${JSON.stringify(k)}, bubbles:true}, ${JSON.stringify(extra)})))`);
const cardText = () => ev(`document.querySelector('.card')?.textContent ?? ''`);
const activeLabel = () => ev(`document.activeElement?.textContent?.trim() ?? ''`);

function killListenersByPort(port) {
  // Only ever test_listener.exe, found by name.
  try {
    const out = execFileSync("powershell", ["-NoProfile", "-Command",
      `Get-NetTCPConnection -State Listen -LocalPort ${port} -ErrorAction SilentlyContinue | ForEach-Object { $p = Get-Process -Id $_.OwningProcess; if ($p.ProcessName -eq 'test_listener') { Stop-Process -Id $p.Id -Force } }`]);
  } catch {}
}

async function main() {
  const port = await freePort();
  const victim = spawn(LISTENER, [String(port), "live test"], { cwd: work, stdio: "ignore" });
  children.push(victim);
  await until("victim listening", () => isListening(port));

  launchApp();
  await connect();
  await until("frontend ready", () => ev(`!!document.querySelector('input.search')`));
  await cdp("Log.enable");
  await cdp("Runtime.enable");
  await sleep(1500);

  // --- kill port: search, preview, bypass attempts, confirm ---
  await typeQuery(`kill port ${port}`);
  await until("port row", async () => (await ev(`document.querySelector('#results')?.innerText ?? ''`)).includes(`Kill port ${port}`));
  const row = await ev(`document.querySelector('#results li')?.innerText`);
  check("search shows the real process for the typed port", row.includes("test_listener.exe") && row.includes(`PID ${victim.pid}`), row.replace(/\n/g, " | "));

  await key("Enter");
  await until("confirm card", async () => (await cardText()).includes("Needs confirmation"));
  const card = await cardText();
  check("card previews the live process (pid, program, command, folder)", card.includes(`PID ${victim.pid}`) && card.includes("test_listener.exe") && card.includes("live test") && card.includes(path.basename(work)), card.replace(/\n+/g, " | ").slice(0, 300));
  await until("presented + focused", async () => (await activeLabel()).startsWith("Confirm"));
  check("Confirm button has focus after the preview is presented", true);
  check("process still alive while the card is open", await isListening(port));

  // Bypass attempts through every command the webview can call.
  const prep = await invoke("prepare_action", { action: "kill_port", id: `port:${port}` });
  check("webview prepare_action returns a confirm-level ticket", prep.ok && prep.v.risk === "confirm");
  const r1 = await invoke("run_action", { ticket: prep.v.ticket });
  check("run_action refuses a confirm ticket", !r1.ok && /needs confirmation/.test(r1.e), r1.e);
  const r2 = await invoke("confirm_action", { ticket: prep.v.ticket });
  check("confirm_action refuses a ticket that was never presented", !r2.ok && /not shown/.test(r2.e), r2.e);
  await invoke("present_action", { ticket: prep.v.ticket });
  const r3 = await invoke("confirm_action", { ticket: prep.v.ticket });
  check("confirm_action refuses an instant confirm after present", !r3.ok && /too quickly/.test(r3.e), r3.e);
  const r4 = await invoke("prepare_action", { action: "kill_port", id: "port:1" });
  check("a port that was not offered cannot be targeted", !r4.ok, r4.e);
  const r5 = await invoke("prepare_action", { action: "open", id: "file:C:\\Windows\\System32\\cmd.exe" });
  check("an arbitrary path cannot be targeted", !r5.ok, r5.e);
  await invoke("cancel_action", { ticket: prep.v.ticket });

  // Keyboard: Enter / Space on the window (not on the button) do nothing.
  await key("Enter");
  await key(" ");
  await sleep(300);
  check("Enter/Space on the window do not confirm", await isListening(port));

  // A script-made click on Confirm is ignored (isTrusted); a real Enter on the focused button works.
  await sleep(500);
  await syntheticClick("Confirm");
  await sleep(500);
  check("a synthetic click on Confirm does nothing", (await isListening(port)) && (await cardText()).includes("Needs confirmation"));
  await realEnter();
  await until("result card", async () => (await cardText()).includes("Stopped"));
  check("button confirm stopped the process", !(await isListening(port)), (await cardText()).replace(/\n+/g, " | "));
  const hist = await invoke("can_undo");
  check("undo is available after the stop", hist.ok && hist.v === true);

  // --- restart the whole app; the log must still allow undo ---
  stopApp();
  await sleep(1500);
  launchApp();
  ws.close();
  await connect();
  await until("frontend ready (2nd launch)", () => ev(`!!document.querySelector('input.search')`));
  await sleep(1500);
  check("after restarting Lookout the undo log still offers undo", (await invoke("can_undo")).v === true);
  await key("z", { ctrlKey: true });
  await until("undo card", async () => (await cardText()).includes("Start again"));
  const ucard = await cardText();
  check("Ctrl+Z (empty box) shows the restart preview with command and folder", ucard.includes("test_listener.exe") && ucard.includes("live test") && ucard.includes(path.basename(work)), ucard.replace(/\n+/g, " | ").slice(0, 300));
  check("nothing restarted before confirming", !(await isListening(port)));
  await until("undo focus", async () => (await activeLabel()).startsWith("Undo"));
  await sleep(500); // a human cannot press within the backend's 350 ms dwell
  await syntheticClick("Undo");
  await sleep(500);
  check("a synthetic click on Undo does nothing", !(await isListening(port)));
  await realClick("Undo"); // a real mouse click
  await until("undo result", async () => (await cardText()).includes("Started again"));
  check("undo restarted the program on the same port", await until("listening", () => isListening(port), 10000));
  console.log("   ", (await cardText()).replace(/\n+/g, " | "));
  killListenersByPort(port);
  await until("port free", async () => !(await isListening(port)));

  // --- open through the gate with the real ShellExecute launcher ---
  await key("Escape"); // close result card
  await typeQuery("live alpha");
  await until("file rows", async () => (await ev(`document.querySelector('#results')?.innerText ?? ''`)).includes("live alpha.txt"));
  // The txt is a safe action: Enter opens it with no card and hides the bar.
  const idx = await ev(`[...document.querySelectorAll('#results li')].findIndex(li => li.innerText.includes('live alpha.txt'))`);
  for (let i = 0; i < idx; i++) await key("ArrowDown");
  await key("Enter");
  await sleep(300);
  check("a document opens without a card (safe)", !(await cardText()).includes("Needs confirmation"));
  const opened = await until("notepad window", () => {
    try {
      const out = execFileSync("powershell", ["-NoProfile", "-Command", "(Get-Process | Where-Object { $_.MainWindowTitle -like '*live alpha.txt*' }).MainWindowTitle"]).toString().trim();
      return out || null;
    } catch { return null; }
  }, 10000).catch(() => null);
  check("ShellExecute opened the document in its real handler", !!opened, String(opened));
  try { execFileSync("powershell", ["-NoProfile", "-Command", "Get-Process | Where-Object { $_.MainWindowTitle -like '*live alpha.txt*' } | ForEach-Object { [void]$_.CloseMainWindow() }"]); } catch {}

  // The .bat needs confirmation; Escape cancels and it does not run.
  await ev(`window.__TAURI_INTERNALS__.invoke('hide_bar')`); await sleep(200);
  await typeQuery("live tool alpha");
  await until("bat row", async () => (await ev(`document.querySelector('#results')?.innerText ?? ''`)).includes("live tool alpha.bat"));
  const bidx = await ev(`[...document.querySelectorAll('#results li')].findIndex(li => li.innerText.includes('live tool alpha.bat'))`);
  for (let i = 0; i < bidx; i++) await key("ArrowDown");
  await key("Enter");
  await until("bat card", async () => (await cardText()).includes("Needs confirmation"));
  check("an executable type shows a confirm card", (await cardText()).includes("Run live tool alpha.bat"));
  await key("Escape");
  await sleep(1500);
  check("Escape cancelled it and the .bat did not run", !fs.existsSync(marker));
  await key("Enter"); // still on the same row
  await until("bat card again", async () => (await cardText()).includes("Needs confirmation"));
  await until("bat focus", async () => (await activeLabel()).startsWith("Confirm"));
  await sleep(500);
  await realEnter();
  await until("bat marker", () => fs.existsSync(marker), 10000).catch(() => {});
  check("after Confirm the .bat ran (marker written)", fs.existsSync(marker));
  check("no console errors or CSP violations in the page", logErrors.length === 0, logErrors.slice(0, 3).join(" ; "));
}

try {
  await main();
} catch (e) {
  console.log("ERROR", e.message);
  results.push({ name: "script", ok: false });
} finally {
  for (const c of children) try { c.kill(); } catch {}
  stopApp();
  const failed = results.filter((r) => !r.ok).length;
  console.log(`\n${results.length - failed} passed, ${failed} failed`);
  try { fs.rmSync(tmp, { recursive: true, force: true }); } catch {}
  process.exit(failed ? 1 : 0);
}
