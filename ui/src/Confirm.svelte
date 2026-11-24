<script lang="ts" module>
  export interface Prepared {
    ticket: string;
    action_id: string;
    label: string;
    risk: "safe" | "confirm" | "dangerous";
    preview: { from: string | null; title: string; lines: string[]; warnings: string[] };
  }
</script>

<script lang="ts">
  // The ordinary confirmation card (prepare -> present -> confirm) for a list the backend built.
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";

  let {
    prepared,
    ondone,
    oncancel,
  }: { prepared: Prepared; ondone: (message: string) => void; oncancel: (error?: string) => void } = $props();

  let presented = $state(false);
  let busy = $state(false);
  let error = $state("");
  let cancelButton: HTMLButtonElement | undefined = $state();

  onMount(async () => {
    await tick();
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    try {
      await invoke("present_action", { ticket: prepared.ticket });
      presented = true;
      await tick();
      cancelButton?.focus(); // a stray Enter cannot confirm a deletion
    } catch (e) {
      oncancel(String(e));
    }
  });

  function cancel() {
    invoke("cancel_action", { ticket: prepared.ticket });
    oncancel();
  }

  async function confirm(e: Event) {
    if (!e.isTrusted || !presented || busy) return;
    busy = true;
    try {
      const d = await invoke<{ message: string }>("confirm_action", { ticket: prepared.ticket });
      ondone(d.message);
    } catch (err) {
      const m = String(err);
      if (m.includes("too quickly")) {
        busy = false;
        error = m;
      } else {
        oncancel(m);
      }
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cancel();
    }
  }
</script>

<svelte:window onkeydowncapture={onKey} />

<div class="scrim">
  <div class="card" role="alertdialog" aria-labelledby="cf-title">
    <span class="label-caps">{prepared.risk === "dangerous" ? "Cannot be undone" : "Needs confirmation"}</span>
    <h3 id="cf-title">{prepared.preview.title}</h3>
    <div class="lines">
      {#each prepared.preview.lines as line}<p class="line">{line}</p>{/each}
    </div>
    {#each prepared.preview.warnings as w}<p class="warning">{w}</p>{/each}
    {#if error}<p class="warning" role="alert">{error}</p>{/if}
    <div class="buttons">
      <button class="btn" bind:this={cancelButton} onclick={cancel}>Cancel <kbd>Esc</kbd></button>
      <button class="btn primary" disabled={!presented || busy} onclick={confirm}>{prepared.label}</button>
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 30;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    padding: 16px 18px;
    width: min(640px, 92%);
    max-height: 86%;
    display: flex;
    flex-direction: column;
  }
  h3 {
    margin: 6px 0 10px;
    font-size: 20px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .lines {
    overflow: auto;
    max-height: 38vh;
    min-height: 0;
  }
  .line {
    margin: 3px 0;
    color: var(--text-soft);
    font-family: var(--font-code);
    font-size: 13px;
    word-break: break-all;
  }
  .warning {
    margin: 8px 0 0;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
</style>
