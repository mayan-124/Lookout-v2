<script lang="ts">
  // What Lookout can do. The rows come from a fixed table in the app; the mode
  // decides which are listed.
  import type { HelpEntry } from "./modes";

  let {
    entries,
    selected = $bindable(),
    onpick,
    ontoggle,
  }: {
    entries: HelpEntry[];
    selected: number;
    onpick: (text: string) => void;
    ontoggle: () => void;
  } = $props();

  let list: HTMLElement | undefined = $state();
  $effect(() => {
    list?.querySelector(`[data-help="${selected}"]`)?.scrollIntoView({ block: "nearest" });
  });
  /** The group heading above entry `i`, if one starts there. */
  function heading(i: number): string | null {
    const e = entries[i];
    const before = entries[i - 1];
    if (!("row" in e)) return null;
    return before && "row" in before && before.row.group === e.row.group ? null : e.row.group;
  }
</script>

<div class="help" bind:this={list}>
  {#each entries as e, i ("row" in e ? e.row.example : "advanced")}
    {#if heading(i)}
      <h3 class="label-caps">{heading(i)}</h3>
    {/if}
    <button
      type="button"
      class="hrow"
      class:fold={!("row" in e)}
      class:selected={i === selected}
      data-help={i}
      aria-expanded={"row" in e ? undefined : e.open}
      onmousemove={() => (selected = i)}
      onfocus={() => (selected = i)}
      onclick={() => ("row" in e ? onpick(e.row.fill) : ontoggle())}
    >
      {#if "row" in e}
        <span class="what">{e.row.what}</span>
        <span class="code example">{e.row.example}</span>
      {:else}
        <span class="label-caps">{e.open ? "▾" : "▸"} Advanced</span>
      {/if}
    </button>
  {/each}
</div>

<style>
  .help {
    max-height: 440px;
    overflow-y: auto;
    padding: 4px 12px 10px;
  }
  h3 {
    margin: 12px 8px 4px;
  }
  .hrow {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 7px 12px;
    border: 1px solid transparent;
    border-radius: var(--r-md);
    font-size: var(--fs-md);
    cursor: pointer;
  }
  .hrow.fold {
    margin-top: 10px;
    justify-content: flex-start;
  }
  .hrow.selected {
    background: var(--accent-soft);
    border-color: var(--accent-line);
  }
  .hrow:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .what {
    min-width: 0;
  }
  .example {
    flex: none;
    color: var(--text-soft);
    font-size: 13px;
  }
</style>
