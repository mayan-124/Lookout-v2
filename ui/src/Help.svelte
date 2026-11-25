<script lang="ts" module>
  export interface HelpRow {
    group: string;
    what: string;
    example: string;
    fill: string;
  }
</script>

<script lang="ts">
  // Everything Lookout can do. The rows come from a fixed table in the app.
  let {
    rows,
    selected = $bindable(),
    onpick,
  }: { rows: HelpRow[]; selected: number; onpick: (text: string) => void } = $props();

  let list: HTMLElement | undefined = $state();
  $effect(() => {
    list?.querySelector(`[data-help="${selected}"]`)?.scrollIntoView({ block: "nearest" });
  });
</script>

<div class="help" bind:this={list}>
  {#each rows as r, i (r.example)}
    {#if i === 0 || rows[i - 1].group !== r.group}
      <h3 class="label-caps">{r.group}</h3>
    {/if}
    <button
      type="button"
      class="hrow"
      class:selected={i === selected}
      data-help={i}
      onmousemove={() => (selected = i)}
      onfocus={() => (selected = i)}
      onclick={() => onpick(r.fill)}
    >
      <span class="what">{r.what}</span>
      <span class="code example">{r.example}</span>
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
