<script lang="ts">
  // The one result row: icon, title, muted detail, and what belongs at the
  // right. Its look is `.result` in app.css.
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";

  let {
    tag = "div",
    selected = false,
    extra = "",
    icon,
    title,
    aside,
    detail,
    end,
    tooltip,
    ...rest
  }: Omit<HTMLButtonAttributes, "title"> & {
    tag?: "div" | "button";
    selected?: boolean;
    /** Further classes, such as `draggable`. */
    extra?: string;
    icon: Snippet;
    title: Snippet;
    /** Beside the title. */
    aside?: Snippet;
    /** The muted second line. */
    detail?: Snippet;
    end?: Snippet;
    tooltip?: string;
    /** Further `data-` attributes. */
    [data: `data-${string}`]: unknown;
  } = $props();
</script>

<svelte:element
  this={tag}
  {...rest}
  type={tag === "button" ? "button" : undefined}
  title={tooltip}
  class={["result", extra]}
  class:selected
>
  {@render icon()}
  <span class="text">
    <span class="l1"><span class="title">{@render title()}</span>{@render aside?.()}</span>
    {@render detail?.()}
  </span>
  {@render end?.()}
</svelte:element>
