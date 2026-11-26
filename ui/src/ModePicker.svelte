<script lang="ts">
  // Three mode cards. Picking one applies and saves it.
  import { MODES, modeLabel, type Mode } from "./modes";
  import { setMode, ui } from "./uistate.svelte";
  import { radioKeys } from "./radiokeys";

  let { label = "Mode" }: { label?: string } = $props();

  let error = $state("");
  const pick = async (mode: Mode) => (error = await setMode(mode));
</script>

<div class="field">
  <span class="fl">
    <span class="ft">{label}: {modeLabel(ui)}</span>
    <span class="hint">Changes only what you see.</span>
  </span>
  <!-- svelte-ignore a11y_interactive_supports_focus (the radios take focus) -->
  <div class="modes" role="radiogroup" aria-label="Mode" onkeydown={radioKeys}>
    {#each MODES as m (m.id)}
      <button
        type="button"
        class="mode"
        role="radio"
        aria-checked={!ui.custom && ui.mode === m.id}
        tabindex={ui.mode === m.id ? 0 : -1}
        onclick={() => pick(m.id)}
      >
        <span class="ft">{m.name}</span>
        <span class="hint">{m.what}</span>
      </button>
    {/each}
  </div>
  {#if error}<span class="err" role="alert">{error}</span>{/if}
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 0;
  }
  .fl {
    display: flex;
    flex-direction: column;
  }
  .ft {
    font-weight: 600;
  }
  .modes {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
  }
  .mode {
    all: unset;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 12px;
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
    background: var(--btn-bg);
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .mode:hover {
    background: var(--btn-bg-hover);
  }
  .mode[aria-checked="true"] {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .mode:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .err {
    color: var(--danger);
    font-size: 12px;
  }
</style>
