<script lang="ts">
  // The living orb. Only transform and opacity animate. With active=false (the
  // window is hidden) or reduced motion, nothing animates at all.
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
    will-change: transform, opacity;
  }
  .core {
    width: 16px;
    height: 16px;
    background: radial-gradient(circle at 35% 30%, #fff 0, var(--accent) 45%, var(--accent) 100%);
    box-shadow: 0 0 10px var(--accent-line);
    animation: breathe 4s ease-in-out infinite;
  }
  .ring {
    inset: 1px;
    border: 1px solid var(--accent-line);
    opacity: 0;
    animation: ring 4s ease-out infinite;
  }
  [data-mode="indexing"] .core {
    animation-duration: 2.4s;
  }
  [data-mode="indexing"] .ring {
    animation-duration: 2.4s;
  }
  /* Results are on screen: the core pops once and an extra ring keeps pulsing. */
  [data-mode="found"] .core {
    animation: found 450ms var(--ease) 1;
  }
  [data-mode="found"] .ring {
    animation: ring 1.6s ease-out infinite;
  }
  [data-mode="error"] .core {
    background: var(--danger);
    box-shadow: 0 0 10px var(--danger);
    animation: shake 300ms linear 1;
  }
  [data-mode="error"] .ring {
    display: none;
  }
  [data-mode="paused"] .core {
    opacity: 0.45;
    animation: none;
  }
  [data-mode="paused"] .ring {
    display: none;
  }
  .still .core,
  .still .ring {
    animation: none;
  }

  @keyframes breathe {
    0%,
    100% {
      transform: scale(0.9);
      opacity: 0.8;
    }
    50% {
      transform: scale(1.1);
      opacity: 1;
    }
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
    0% {
      transform: scale(1);
      opacity: 0.8;
    }
    40% {
      transform: scale(1.4);
      opacity: 1;
    }
    100% {
      transform: scale(1);
      opacity: 1;
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
