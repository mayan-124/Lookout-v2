<script lang="ts">
  // The setup's "Daily features" rows. The setup's primary button calls
  // `turnOn`; it allows the ticked add-ons only, with the permissions listed here.
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { CANNOT_TURN_ON, CLIPBOARD_ID, DAILY, IMAGES, startTicks } from "./addons";
  import { plain as errorText } from "./errors";

  interface Addon {
    id: string;
    enabled: boolean;
    approved: boolean;
    permissions: string[];
    config: Record<string, number | string>;
  }

  let {
    rerun,
    busy,
    ready = $bindable(false),
  }: {
    /** The setup was run again from Settings: nothing new is ticked. */
    rerun: boolean;
    busy: boolean;
    /** Something is ticked or chosen that "Turn on ticked" would change. */
    ready?: boolean;
  } = $props();

  let addons: Addon[] = $state([]);
  let ticks: Record<string, boolean> = $state({});
  let images = $state(false);
  let engine = $state("off");
  let savedEngine = $state("off");
  let loaded = $state(false);
  let open: Record<string, boolean> = $state({});
  let errors: Record<string, string> = $state({});

  const addon = (id: string) => addons.find((p) => p.id === id);
  // On means turned on with the permissions as they are now.
  const isOn = (id: string) => !!addon(id)?.enabled && !!addon(id)?.approved;
  const imagesOn = $derived(addon(CLIPBOARD_ID)?.config.keep_images === "on");
  const wanted = $derived(DAILY.filter((o) => ticks[o.id] && addon(o.id) && !isOn(o.id)));
  const wantsImages = $derived(images && !imagesOn && !!ticks[CLIPBOARD_ID] && !!addon(CLIPBOARD_ID));
  $effect(() => {
    ready = loaded && (wanted.length > 0 || wantsImages || engine !== savedEngine);
  });

  onMount(async () => {
    try {
      const [list, saved] = await Promise.all([
        invoke<Addon[]>("plugins_list"),
        invoke<{ web_search: string }>("privacy_state"),
      ]);
      addons = list;
      ticks = startTicks(rerun, isOn);
      images = imagesOn;
      engine = savedEngine = saved.web_search;
    } catch (e) {
      errors.list = errorText(e);
    }
    loaded = true;
  });

  /** Turns on what is ticked, one at a time. True when nothing failed. */
  export async function turnOn(): Promise<boolean> {
    errors = {};
    const pictures = wantsImages;
    for (const o of wanted) {
      try {
        addons = await invoke<Addon[]>("plugin_turn_on", { id: o.id });
      } catch {
        errors[o.id] = CANNOT_TURN_ON;
      }
    }
    if (pictures && isOn(CLIPBOARD_ID)) {
      try {
        addons = await invoke<Addon[]>("set_keep_images", { on: true });
      } catch {
        errors.images = CANNOT_TURN_ON;
      }
    }
    if (engine !== savedEngine) {
      try {
        await invoke("set_web_search", { engine });
        savedEngine = engine;
      } catch (e) {
        errors.web = errorText(e);
      }
    }
    // A failed one shows as off, whatever else happened.
    addons = await invoke<Addon[]>("plugins_list").catch(() => addons);
    return Object.keys(errors).length === 0;
  }
</script>

{#each DAILY as o (o.id)}
  {@const p = addon(o.id)}
  <div class="daily">
    <div class="line">
      <label class="pick">
        <input
          type="checkbox"
          checked={isOn(o.id) || !!ticks[o.id]}
          disabled={busy || !p || isOn(o.id)}
          onchange={(e) => (ticks[o.id] = e.currentTarget.checked)}
        />
        <span class="fl">
          <span class="ft">{o.name}{#if isOn(o.id)} <span class="tag">On</span>{/if}</span>
          <span class="hint">{o.what} {o.privacy}</span>
        </span>
      </label>
      {#if p}
        <button type="button" class="btn small text" aria-expanded={!!open[o.id]} onclick={() => (open[o.id] = !open[o.id])}
          >{open[o.id] ? "▾" : "▸"} What it can do</button
        >
      {/if}
    </div>
    {#if p && open[o.id]}
      <ul class="hint perms">
        {#each p.permissions as perm}<li>{perm}</li>{/each}
      </ul>
    {/if}
    {#if loaded && !p}<p class="hint">Not available right now.</p>{/if}
    {#if errors[o.id]}<p class="note" role="alert">{errors[o.id]}</p>{/if}
  </div>
{/each}
<div class="daily sub">
  <label class="pick">
    <input
      type="checkbox"
      checked={imagesOn || (images && !!ticks[CLIPBOARD_ID])}
      disabled={busy || imagesOn || !ticks[CLIPBOARD_ID] || !addon(CLIPBOARD_ID)}
      onchange={(e) => (images = e.currentTarget.checked)}
    />
    <span class="fl">
      <span class="ft">{IMAGES.name}</span>
      <span class="hint">{IMAGES.privacy}</span>
    </span>
  </label>
  {#if errors.images}<p class="note" role="alert">{errors.images}</p>{/if}
</div>
<div class="daily">
  <label class="line">
    <span class="fl">
      <span class="ft">Web search</span>
      <span class="hint">Type g and your words. Nothing is sent until you press Enter.</span>
    </span>
    <select bind:value={engine} disabled={busy || !loaded}>
      <option value="off">Off</option>
      <option value="duckduckgo">DuckDuckGo</option>
      <option value="google">Google</option>
      <option value="bing">Bing</option>
    </select>
  </label>
  {#if errors.web}<p class="note" role="alert">{errors.web}</p>{/if}
</div>
{#if errors.list}<p class="note" role="alert">{errors.list}</p>{/if}

<style>
  .daily {
    padding: 8px 0;
    border-top: 1px solid var(--border);
  }
  .daily.sub {
    padding-left: 26px;
  }
  .line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
  }
  .pick {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    cursor: pointer;
  }
  .line > button {
    flex: none;
  }
  .fl {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .ft {
    font-weight: 600;
  }
  .perms {
    margin: 4px 0 0;
    padding-left: 44px;
  }
  .daily p {
    margin: 4px 0 0;
  }
</style>
