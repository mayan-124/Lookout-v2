import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SIZE_EXAMPLES, completions, operatorGhost, removeOperator } from "../src/operators.ts";
import { tipsFor } from "../src/modes.ts";
import data from "../src/registry.json" with { type: "json" };

const VALUES = { extensions: ["pdf", "png", "txt", "docx"], folders: ["Downloads", "Documents", "My Projects"] };
const labels = (q) => completions(q, VALUES).map((c) => c.label);
const SHOW = { disk: true, clipboard: true, capture: true, timers: true, quick_keys: true, filters: true };

test("values are offered for the operator at the end of the box", () => {
  assert.deepEqual(labels("ext:"), ["ext:pdf", "ext:png", "ext:txt", "ext:docx"]);
  assert.deepEqual(labels("report EXT:p"), ["ext:pdf", "ext:png"]);
  assert.deepEqual(labels("size:"), SIZE_EXAMPLES.map((s) => `size:${s}`));
  assert.deepEqual(labels("size:>1"), ["size:>1mb", "size:>10mb", "size:>100mb", "size:>1gb"]);
  assert.deepEqual(labels("in:do"), ["in:Downloads", "in:Documents"]);
  assert.equal(completions("report ext:pd", VALUES)[0].text, "report ext:pdf");
});

test("lists and quoted folders complete too", () => {
  assert.equal(completions("ext:pdf,d", VALUES)[0].text, "ext:pdf,docx");
  assert.ok(!labels("ext:pdf,").includes("ext:pdf"));
  assert.equal(completions("in:my", VALUES)[0].text, 'in:"My Projects"');
  assert.equal(completions('tax in:"My P', VALUES)[0].text, 'tax in:"My Projects"');
});

test("nothing is offered elsewhere", () => {
  for (const q of ["", "report", "name:rep", "ext:pdf", "size:>1mb", "in:Downloads", "ext:pdf budget", "ext:zz", "bin:", 'in:"My Projects"']) {
    assert.deepEqual(labels(q), [], q);
  }
  assert.deepEqual(completions("ext:", { extensions: [], folders: [] }), []);
});

test("ghost text continues what was typed", () => {
  const ghost = (q) => operatorGhost(q, completions(q, VALUES));
  assert.equal(ghost("ext:p"), "df");
  assert.equal(ghost("in:down"), "loads");
  // A quote has to go before what was typed: the list shows it, no ghost.
  assert.equal(ghost("in:my"), "");
  assert.equal(ghost("report"), "");
});

test("a chip removes its own operator only", () => {
  assert.equal(removeOperator("report ext:pdf size:>1mb", "ext:pdf"), "report size:>1mb");
  assert.equal(removeOperator("ext:pdf", "ext:pdf"), "");
  assert.equal(removeOperator('tax  in:"My Projects" 2025', 'in:"My Projects"'), "tax 2025");
  assert.equal(removeOperator("ext:pdfx ext:pdf", "ext:pdf"), "ext:pdfx");
  assert.equal(removeOperator("report", "ext:pdf"), "report");
});

test("the operators are in help for every list, with one tip outside Simple", () => {
  const ops = data.filter((a) => a.id.startsWith("op_"));
  assert.deepEqual(ops.map((a) => a.id), ["op_ext", "op_size", "op_in", "op_name"]);
  for (const mode of ["standard", "full"]) {
    assert.deepEqual(tipsFor(ops, mode, SHOW).map((a) => a.id), ["op_ext"], mode);
  }
  assert.deepEqual(tipsFor(ops, "simple", SHOW), []);
});

test("a size search says when sizes are still being collected", () => {
  const app = readFileSync(new URL("../src/App.svelte", import.meta.url), "utf8");
  assert.match(app, /sizes_pending && typedSize\(\)\}<p class="readline muted" role="status">Sizes are still being collected</);
});

test("the preview hint says where Space works", () => {
  const panel = readFileSync(new URL("../src/Preview.svelte", import.meta.url), "utf8");
  assert.match(panel, /Click here, then <kbd>Space<\/kbd> for a larger preview/);
});

test("the window shows chips and suggestions outside Simple only", () => {
  const app = readFileSync(new URL("../src/App.svelte", import.meta.url), "utf8");
  assert.match(app, /ui\.mode !== "simple"/);
  assert.match(app, /removeOperator\(query, /);
});
