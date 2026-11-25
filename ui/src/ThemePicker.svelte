<script lang="ts">
  // The theme grid and the accent choice. Both apply and save as they are picked.
  import { ACCENT_PRESETS, THEMES, normalizeHex, themeById } from "./theme";
  import { look, previewAccent, setAccent, setTheme } from "./look.svelte";

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
  // Arrow keys move and pick within a radio group.
  function radioKeys(e: KeyboardEvent) {
    const step = ["ArrowRight", "ArrowDown"].includes(e.key) ? 1 : ["ArrowLeft", "ArrowUp"].includes(e.key) ? -1 : 0;
    const from = e.target as HTMLElement;
    if (!step || from.getAttribute("role") !== "radio") return;
    e.preventDefault();
    const all = [...from.closest('[role="radiogroup"]')!.querySelectorAll<HTMLButtonElement>('[role="radio"]')];
    const next = all[(all.indexOf(from as HTMLButtonElement) + step + all.length) % all.length];
    next.focus();
    next.click();
  }
</script>

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
        <span>{t.name}</span>
        <span class="dots" aria-hidden="true">
          {#each [t.base, t.tint1, t.tint2, t.accent] as color}
            <i style:background={color}></i>
          {/each}
        </span>
      </button>
    {/each}
  </div>
</div>
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
  {#if look.saveError}<span class="err" role="alert">Not saved: {look.saveError}</span>{/if}
</div>

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
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 8px;
  }
  .theme {
    all: unset;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 12px;
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
  .dots {
    display: flex;
    gap: 6px;
  }
  .dots i {
    width: 12px;
    height: 12px;
    border: 1px solid var(--hover-border);
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
