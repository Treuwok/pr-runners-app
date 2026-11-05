<script lang="ts">
  // 排隊中的 job 只列一次：依 workflow 歸組，同名的合併成「×n」，名稱保留 GitHub 原名。
  import { openUrl } from "$lib/store.svelte";
  import { NO_MATCH, formatElapsed, jobText, repoName, type QueueGroup } from "$lib/view";

  let { groups, total, dominant }: { groups: QueueGroup[]; total: number; dominant: string | null } = $props();
  const oldest = $derived(groups[0]?.oldest ?? null);
  const attention = $derived(groups.some((g) => g.attention));
  // 只有一個 workflow 且 repo 就是頂端那個時，標題已經沒有資訊量
  const showHeads = $derived(groups.length > 1 || groups.some((g) => repoName(g.repo) !== dominant));
</script>

{#if total > 0}
  <section class="queue" aria-label="排隊中">
    <div class="qh">
      <b>排隊 {total}</b>
      <span class:warn={attention}>最久 {formatElapsed(oldest)}</span>
    </div>
    <div class="card">
      {#each groups as g (g.key)}
        {#if showHeads}
          <div class="qw">
            <span>{jobText({ repo: g.repo, job: g.workflow }, dominant)}</span>
            <span class:warn={g.attention}>{formatElapsed(g.oldest)}</span>
          </div>
        {/if}
        {#each g.jobs as j (`${j.name}\u0000${j.wait}`)}
          {#if j.url}
            <button class="qj" onclick={() => openUrl(j.url!)}>
              <span>{j.name} <span class="x">{j.wait}</span></span>
              {#if j.noMatch}<span class="tagwarn">{NO_MATCH}</span>{/if}
            </button>
          {:else}
            <div class="qj">
              <span>{j.name} <span class="x">×{j.count} · {j.wait}</span></span>
              {#if j.noMatch}<span class="tagwarn">{NO_MATCH}</span>{/if}
            </div>
          {/if}
        {/each}
      {/each}
    </div>
  </section>
{/if}
