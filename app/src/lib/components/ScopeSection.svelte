<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ScopeView } from "$lib/generated/ScopeView";
  import { BUSY_ONLY_NOTE, NO_RUNNERS, splitRunners } from "$lib/view";
  import Lane from "./Lane.svelte";
  import OfflineList from "./OfflineList.svelte";
  import PoolCard from "./PoolCard.svelte";

  let {
    scope,
    dominant,
    maxSecs,
    hues,
    showTitle = true,
    limit,
    onmore,
    onopen,
    children,
  }: {
    scope: ScopeView;
    dominant: string | null;
    maxSecs: number;
    hues: Map<string, number>;
    showTitle?: boolean;
    /** 小視窗只放前幾條自家機器的忙碌泳道，其餘收成一行「另外 N 台」 */
    limit?: number;
    onmore?: () => void;
    onopen: (key: string) => void;
    /** 兩欄時放在右欄（臨時機器下面）的內容，例如排隊 */
    children?: Snippet;
  } = $props();
  const split = $derived(splitRunners(scope.runners));
  const busy = $derived(limit === undefined ? split.busy : split.busy.slice(0, limit));
  const hidden = $derived(split.busy.length - busy.length);
  const ownOnline = $derived(split.busy.length + split.idle.length);
  const poolBusy = $derived(split.pools.reduce((n, g) => n + g.busy, 0));
  const empty = $derived(
    split.busy.length + split.idle.length + split.offline.length === 0 && split.pools.length === 0,
  );
</script>

{#if empty && scope.roster === "Full"}
  <p class="empty">{scope.name} · {NO_RUNNERS}</p>
{:else}
  <section class="scope">
    {#if showTitle}
      <div class="hd">
        <b>{scope.name}</b>
        <span>{ownOnline} 台在線</span>
      </div>
    {/if}
    <div class="cols" class:two={split.pools.length > 0}>
      <div class="col own">
        {#if busy.length + split.idle.length > 0}
          <div class="sec">
            <b>自家機器</b><span>self-hosted · {ownOnline} 台</span>
          </div>
        {/if}
        {#if busy.length > 0}
          <div class="card">
            {#each busy as r (r.key)}
              <Lane runner={r} {dominant} {maxSecs} {hues} {onopen} />
            {/each}
            {#if hidden > 0}
              <button class="more" onclick={onmore}>另外 {hidden} 台…</button>
            {/if}
          </div>
        {/if}
        {#if split.idle.length > 0}
          <p class="idles">
            <span class="idle-label">閒置 {split.idle.length}</span>
            {#each split.idle as r (r.key)}
              <button class="idle" title="查看執行歷史" onclick={() => onopen(r.key)}>
                <i class="dot idle" aria-hidden="true"></i>{r.name}
              </button>
            {/each}
          </p>
        {/if}
      </div>
      {#if split.pools.length > 0}
        <div class="col pools">
          <div class="sec">
            <b>臨時機器</b><span>依名稱自動辨識 · {poolBusy} 台在跑</span>
          </div>
          {#each split.pools as g (g.tag.stem)}
            <PoolCard group={g} {dominant} {hues} />
          {/each}
          {@render children?.()}
        </div>
      {/if}
      <div class="col foot">
        <OfflineList runners={split.offline} {onopen} />
      </div>
    </div>
    {#if scope.roster === "BusyOnly"}
      <p class="note">{BUSY_ONLY_NOTE}</p>
    {/if}
  </section>
{/if}
