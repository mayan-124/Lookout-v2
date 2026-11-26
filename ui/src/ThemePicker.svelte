<script lang="ts">
  // The theme grid and the accent choice. Both apply and save as they are picked.
  import { ACCENT_PRESETS, THEMES, normalizeHex, themeById } from "./theme";
  import { look, previewAccent, setAccent, setTheme } from "./look.svelte";
  import { radioKeys } from "./radiokeys";

  let { part = "all" }: { part?: "all" | "theme" | "accent" } = $props();

  // The accent as chosen (the theme's own when there is no custom one).
  const chosenAccent = $derived(look.accent ?? themeById(look.theme).accent);
  const presetPicked = $derived(ACCENT_PRESETS.some((a) => a.value === look.accent));
  let hexText = $state("");
  let hexError = $state("");
  $effect(() => {
    hexText = chosenAccent;
    hexError = "";
  });
  function enterHex() {
    const value = normalizeHex(hexText);
    if (!value) {
      hexError = "Enter a color as #RRGGBB.";
      return;
    }
    hexError = "";
    hexText = value;
    setAccent(value);
  }
</script>

{#if part !== "accent"}
<div class="field">
  <span class="fl">
    <span class="ft">Theme</span>
    <span class="hint">Applies as you pick it.</span>
  </span>
  <!-- svelte-ignore a11y_interactive_supports_focus (the radios take focus) -->
  <div class="themes" role="radiogroup" aria-label="Theme" onkeydown={radioKeys}>
    {#each THEMES as t (t.id)}
      <button
        type="button"
        class="theme"
        role="radio"
        aria-checked={look.theme === t.id}
        tabindex={look.theme === t.id ? 0 : -1}
        onclick={() => setTheme(t.id)}
      >
        <span class="sw" aria-hidden="true" style:background={t.panel}>
          <i class="line" style:background={t.text}></i>
          <i class="line short" style:background={t.muted}></i>
          <i class="dot" style:background={t.accent}></i>
        </span>
        <span>{t.name}</span>
      </button>
    {/each}
  </div>
</div>
{/if}
{#if part !== "theme"}
<div class="field">
  <span class="fl">
    <span class="ft">Accent</span>
    <span class="hint">The highlight color.</span>
  </span>
  <div class="accents">
    <!-- svelte-ignore a11y_interactive_supports_focus (the radios take focus) -->
    <div class="swatches" role="radiogroup" aria-label="Accent color" onkeydown={radioKeys}>
      <button
        type="button"
        class="btn small themeaccent"
        role="radio"
        aria-checked={look.accent === null}
        tabindex={presetPicked ? -1 : 0}
        onclick={() => setAccent(null)}>Theme accent</button
      >
      {#each ACCENT_PRESETS as a}
        <button
          type="button"
          class="swatch"
          role="radio"
          aria-checked={look.accent === a.value}
          tabindex={look.accent === a.value ? 0 : -1}
          aria-label={a.name}
          title={a.name}
          style:background={a.value}
          onclick={() => setAccent(a.value)}
        ></button>
      {/each}
    </div>
    <input
      type="color"
      class="picker"
      aria-label="Custom accent color"
      title="Custom color"
      value={chosenAccent.toLowerCase()}
      oninput={(e) => previewAccent(e.currentTarget.value)}
      onchange={(e) => setAccent(e.currentTarget.value)}
    />
    <input
      type="text"
      class="code hex"
      maxlength="7"
      spellcheck="false"
      aria-label="Custom accent hex value"
      aria-invalid={!!hexError}
      bind:value={hexText}
      onkeydown={(e) => e.key === "Enter" && enterHex()}
      onchange={enterHex}
    />
  </div>
  {#if hexError}<span class="err" role="alert">{hexError}</span>{/if}
  {#if look.lightened}<span class="hint" role="status">Lightened so it stays readable on this theme.</span>{/if}
</div>
{/if}
{#if look.saveError}<span class="err" role="alert">Not saved: {look.saveError}</span>{/if}

<style>
  .field {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
  }
  .field:last-child {
    border-bottom: 0;
  }
  .fl {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .ft {
    font-weight: 600;
  }
  .themes {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
  }
  .theme {
    all: unset;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
    background: var(--btn-bg);
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .theme:hover {
    background: var(--btn-bg-hover);
  }
  .theme[aria-checked="true"],
  .themeaccent[aria-checked="true"] {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .theme:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .sw {
    position: relative;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 6px;
    height: 44px;
    padding: 0 10px;
    border: 1px solid var(--hover-border);
    border-radius: var(--r-sm);
  }
  .line {
    width: 45%;
    height: 4px;
    border-radius: var(--r-pill);
  }
  .line.short {
    width: 30%;
  }
  .dot {
    position: absolute;
    right: 10px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
  }
  .accents {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
  }
  .swatches {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .picker {
    box-sizing: border-box;
    width: 40px;
    height: 30px;
    padding: 2px;
    border: 1px solid var(--glass-border);
    border-radius: var(--r-sm);
    background: var(--field-bg);
    cursor: pointer;
  }
  .hex {
    width: 100px;
  }
  .swatch {
    all: unset;
    box-sizing: border-box;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    cursor: pointer;
    border: 2px solid transparent;
  }
  .swatch[aria-checked="true"] {
    border-color: var(--text);
    box-shadow: 0 0 0 2px var(--solid-bg) inset;
  }
  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .err {
    color: var(--danger);
    font-size: 12px;
  }
</style>
