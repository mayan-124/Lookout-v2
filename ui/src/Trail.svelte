<script lang="ts">
  // The top line of every screen below Home: Back, then where you are.
  let { path, onback }: { path: string[]; onback: () => void } = $props();
</script>

<nav class="trail" aria-label="Breadcrumb">
  <button type="button" class="btn small" title="Esc" onclick={onback}>← Back</button>
  <span class="where">{path.filter(Boolean).join(" › ")}</span>
</nav>

<style>
  .trail {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    min-width: 0;
  }
  .where {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 13px;
    opacity: 0.75;
  }
</style>
