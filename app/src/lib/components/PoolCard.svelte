<script lang="ts">
  // 一組臨時機器一張卡：組名＋機型、幾台在跑、一台一格、在跑的 job（不顯示任何亂數 runner 名稱）。
  import { openUrl } from "$lib/store.svelte";
  import { formatElapsed, jobText, poolLabel, type PoolGroup } from "$lib/view";

  const MAX_JOBS = 4;
  let { group, dominant, hues }: { group: PoolGroup; dominant: string | null; hues: Map<string, number> } = $props();
  const shown = $derived(group.jobs.slice(0, MAX_JOBS));
  const hiddenJobs = $derived(group.jobs.slice(MAX_JOBS).reduce((n, j) => n + j.count, 0));
  const cells = $derived([
    ...Array.from({ length: group.busy }, () => "busy"),
    ...Array.from({ length: group.idle }, () => "idle"),
  ]);
</script>

<div class="pool" role="group" aria-label={`${poolLabel(group.tag)}，${group.busy} 台在跑`}>
  <div class="ph">
    <span class="pn">
      <b>{group.tag.name}</b>
      {#if group.tag.machine}<span class="chip">{group.tag.machine}</span>{/if}
    </span>
    <span class="pc">{group.busy} 台在跑</span>
  </div>
  <div class="cells" aria-hidden="true">
    {#each cells as c, i (i)}<i class="cell {c}"></i>{/each}
  </div>
  {#each shown as j (`${j.job.repo}\u0000${j.job.job}`)}
    {@const text = jobText(j.job, dominant)}
    {#snippet body()}
      <span class="pj">
        <i class="rdot rc{hues.get(j.job.repo.split('/').pop() ?? '') ?? 0}" aria-hidden="true"></i>
        <span>{text}{#if j.count > 1} <span class="x">×{j.count}</span>{/if}</span>
      </span>
      <span class="pt">{formatElapsed(j.elapsed)}</span>
    {/snippet}
    {#if j.url}
      <button class="prow" onclick={() => openUrl(j.url!)}>{@render body()}</button>
    {:else}
      <div class="prow">{@render body()}</div>
    {/if}
  {/each}
  {#if hiddenJobs > 0}<p class="pmore">另外 {hiddenJobs} 個 job</p>{/if}
</div>
