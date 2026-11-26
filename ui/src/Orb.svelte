<script lang="ts">
  // The orb: a static flat dot that animates only on events (results arrive,
  // indexing, error), transform and opacity only. With active=false (the window
  // is hidden) or reduced motion, nothing animates at all.
  type Mode = "idle" | "indexing" | "found" | "error" | "paused";
  let { mode = "idle", active = true }: { mode?: Mode; active?: boolean } = $props();
</script>

<span class="orb" data-mode={mode} class:still={!active} aria-hidden="true">
  <i class="ring"></i>
  <i class="core"></i>
</span>

<style>
  .orb {
    position: relative;
    flex: none;
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
  }
  .core,
  .ring {
    position: absolute;
    border-radius: 50%;
  }
  .core {
    width: 16px;
    height: 16px;
    background: var(--accent);
  }
  .ring {
    inset: 1px;
    border: 1px solid var(--accent-line);
    opacity: 0;
  }
  [data-mode="indexing"] .ring {
    animation: ring 3s ease-out infinite;
  }
  /* Results arrived: one pulse. */
  [data-mode="found"] .core {
    animation: found 450ms var(--ease) 1;
  }
  [data-mode="error"] .core {
    background: var(--danger);
    animation: shake 300ms linear 1;
  }
  [data-mode="paused"] .core {
    opacity: 0.45;
  }
  .still .core,
  .still .ring {
    animation: none;
  }

  @keyframes ring {
    0% {
      transform: scale(0.6);
      opacity: 0.35;
    }
    100% {
      transform: scale(1.35);
      opacity: 0;
    }
  }
  @keyframes found {
    0%,
    100% {
      transform: scale(1);
    }
    40% {
      transform: scale(1.4);
    }
  }
  @keyframes shake {
    0%,
    100% {
      transform: translateX(0);
    }
    20% {
      transform: translateX(-3px);
    }
    40% {
      transform: translateX(3px);
    }
    60% {
      transform: translateX(-2px);
    }
    80% {
      transform: translateX(2px);
    }
  }
</style>
