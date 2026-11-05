<script lang="ts">
  // 底部「離線 N 台 ›」：離線不警告、不排到前面，要找的時候點開才列出名稱與離線多久。只算自家機器。
  import type { RunnerView } from "$lib/generated/RunnerView";
  import { laneText } from "$lib/view";

  let { runners, onopen }: { runners: RunnerView[]; onopen: (key: string) => void } = $props();
  let open = $state(false);
</script>

{#if runners.length > 0}
  <div class="offline">
    <button class="offbtn" aria-expanded={open} onclick={() => (open = !open)}>
      離線 {runners.length} 台 <span aria-hidden="true">{open ? "⌄" : "›"}</span>
    </button>
    {#if open}
      <div class="card">
        {#each runners as r (r.key)}
          <button class="offrow" title="查看執行歷史" onclick={() => onopen(r.key)}>
            <span class="who"><span class="dot offline" aria-hidden="true"></span><span class="nm">{r.name}</span></span>
            <span class="quiet">{laneText(r, null)}</span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
{/if}
