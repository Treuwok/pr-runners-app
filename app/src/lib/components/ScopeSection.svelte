<script lang="ts">
  import type { ScopeView } from "$lib/generated/ScopeView";
  import { BUSY_ONLY_NOTE, NO_RUNNERS, maxElapsedSecs, sortRunners } from "$lib/view";
  import Lane from "./Lane.svelte";

  let {
    scope,
    dominant,
    maxSecs,
    hues,
    showTitle = true,
    limit,
    onmore,
    onopen,
  }: {
    scope: ScopeView;
    dominant: string | null;
    maxSecs: number;
    hues: Map<string, number>;
    showTitle?: boolean;
    /** 小視窗只放前幾條泳道，其餘收成一行「另外 N 台」 */
    limit?: number;
    onmore?: () => void;
    onopen: (key: string) => void;
  } = $props();
  const all = $derived(sortRunners(scope.runners));
  const runners = $derived(limit === undefined ? all : all.slice(0, limit));
  const hidden = $derived(all.length - runners.length);
</script>

{#if all.length === 0 && scope.roster === "Full"}
  <p class="empty">{scope.name} · {NO_RUNNERS}</p>
{:else}
  <section class="scope">
    {#if showTitle}
      <div class="hd">
        <b>{scope.name}</b>
        <span>{all.length} 台</span>
      </div>
    {/if}
    <div class="card">
      {#each runners as r (r.key)}
        <Lane runner={r} {dominant} {maxSecs} {hues} {onopen} />
      {/each}
      {#if hidden > 0}
        <button class="more" onclick={onmore}>另外 {hidden} 台…</button>
      {/if}
    </div>
    {#if scope.roster === "BusyOnly"}
      <p class="note">{BUSY_ONLY_NOTE}</p>
    {/if}
  </section>
{/if}
