<script lang="ts" module>
  export type Ico = "app" | "folder" | "image" | "clipboard" | "file" | "rec" | "gif" | "scan" | "timer" | "moon" | "sun" | "gear" | "globe" | "network" | "key" | "warn" | "dot" | "calendar" | "clock" | "note" | "search" | "disk" | "open" | "pin";
</script>

<script lang="ts">
  // The line icons of result rows and Disk rows.
  let { name }: { name: Ico } = $props();
</script>

<svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true">
  {#if name === "app"}
    <rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" /><rect x="9" y="2.5" width="4.5" height="4.5" rx="1" /><rect x="2.5" y="9" width="4.5" height="4.5" rx="1" /><rect x="9" y="9" width="4.5" height="4.5" rx="1" />
  {:else if name === "folder"}
    <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.6l1.4 1.5h5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z" />
  {:else if name === "image"}
    <rect x="2" y="3" width="12" height="10" rx="1.6" /><circle cx="5.7" cy="6.5" r="1.1" /><path d="M2.5 12l3.6-3.4 2.6 2.4 2-1.8 3.3 2.8" />
  {:else if name === "rec"}
    <circle cx="8" cy="8" r="5.5" /><circle cx="8" cy="8" r="2.4" fill="currentColor" />
  {:else if name === "gif"}
    <rect x="1.8" y="3.5" width="12.4" height="9" rx="1.8" /><path d="M6.6 6.6a1.6 1.6 0 1 0 0 2.8V8.2H5.8M8.8 6.4v3.2M10.4 9.6V6.4h1.7M10.4 8h1.3" />
  {:else if name === "scan"}
    <path d="M2 5.5V3.5A1.5 1.5 0 0 1 3.5 2h2M10 2h2.5A1.5 1.5 0 0 1 14 3.5v2M14 10.5v2a1.5 1.5 0 0 1-1.5 1.5H10M6 14H3.5A1.5 1.5 0 0 1 2 12.5v-2M5.5 6h5M8 6v4.5" />
  {:else if name === "timer"}
    <circle cx="8" cy="9" r="5" /><path d="M8 9V6.4M6.5 2h3M12 4l.9-.9" />
  {:else if name === "calendar"}
    <path d="M2.5 3.5h11V14h-11zM2.5 7h11M5.5 2v3M10.5 2v3" />
  {:else if name === "clock"}
    <circle cx="8" cy="8" r="5.8" /><path d="M8 4.5V8l2.5 1.5" />
  {:else if name === "note"}
    <path d="M3 2.5h10v8L9.5 14H3zM13 10.5H9.5V14M5.5 5.5h5M5.5 8h3" />
  {:else if name === "moon"}
    <path d="M13 9.3A5.5 5.5 0 0 1 6.7 3a5.5 5.5 0 1 0 6.3 6.3z" />
  {:else if name === "sun"}
    <circle cx="8" cy="8" r="2.6" /><path d="M8 1.8v1.5M8 12.7v1.5M1.8 8h1.5M12.7 8h1.5M3.6 3.6l1 1M11.4 11.4l1 1M3.6 12.4l1-1M11.4 4.6l1-1" />
  {:else if name === "gear"}
    <circle cx="8" cy="8" r="2" /><path d="M8 1.8l1 1.6 1.9-.4.5 1.8 1.8.6-.5 1.9L14 8l-1.3 1.2.5 1.9-1.8.6-.5 1.8-1.9-.4L8 14.2l-1-1.6-1.9.4-.5-1.8-1.8-.6.5-1.9L2 8l1.3-1.2-.5-1.9 1.8-.6.5-1.8 1.9.4z" />
  {:else if name === "globe"}
    <circle cx="8" cy="8" r="5.8" /><path d="M2.2 8h11.6M8 2.2c2 1.7 2.8 3.6 2.8 5.8S10 12.1 8 13.8C6 12.1 5.2 10.2 5.2 8S6 3.9 8 2.2z" />
  {:else if name === "network"}
    <rect x="6" y="2" width="4" height="3" rx=".8" /><rect x="1.8" y="11" width="4" height="3" rx=".8" /><rect x="10.2" y="11" width="4" height="3" rx=".8" /><path d="M8 5v3M4 11V8h8v3" />
  {:else if name === "key"}
    <circle cx="5.2" cy="10.8" r="2.7" /><path d="M7.2 8.8l6.3-6.3M11 5l1.8 1.8M9.3 6.7l1.4 1.4" />
  {:else if name === "warn"}
    <path d="M8 2.2L14.2 13H1.8z" /><path d="M8 6.5v3M8 11.3v.1" />
  {:else if name === "dot"}
    <circle cx="8" cy="8" r="1.6" fill="currentColor" />
  {:else if name === "clipboard"}
    <rect x="3.5" y="3" width="9" height="10.5" rx="1.5" /><path d="M6 3V2h4v1M5.8 7h4.4M5.8 9.5h4.4" />
  {:else if name === "search"}
    <circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5L14 14" />
  {:else if name === "open"}
    <path d="M6.5 3.5H3.5v9h9v-3M9.5 3h3.5v3.5M13 3L7.8 8.2" />
  {:else if name === "pin"}
    <path d="M8 2.3l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11.1 4.5 13l.8-3.9-2.9-2.7 3.9-.5z" />
  {:else if name === "disk"}
    <circle cx="8" cy="8" r="5.5" /><path d="M8 8V2.5M8 8l4 3" />
  {:else}
    <path d="M4 2h5l3 3v9H4z" /><path d="M9 2v3h3" />
  {/if}
</svg>
