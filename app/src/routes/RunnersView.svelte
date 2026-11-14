<script lang="ts">
  // Mac 小視窗、Mac 主視窗與 Android 的 Runners 頁共用同一個內容：
  // 頂端一行（repo＋計數）→ 每個 scope 一段泳道 → 排隊（只列一次）→ 底部狀態。
  import type { Snapshot } from "$lib/generated/Snapshot";
  import QueueSection from "$lib/components/QueueSection.svelte";
  import ScopeSection from "$lib/components/ScopeSection.svelte";
  import StatusFooter from "$lib/components/StatusFooter.svelte";
  import { isLoading } from "$lib/loading";
  import { countVisible, dominantRepo, groupQueue, maxElapsedSecs, repoHues, repoName, splitRunners } from "$lib/view";

  let {
    snapshot,
    scope = null,
    onrelogin,
    limit,
    onmore,
    onopen,
  }: {
    snapshot: Snapshot;
    scope?: string | null;
    onrelogin: () => void;
    limit?: number;
    onmore?: () => void;
    /** 點某台 runner：打開它的執行歷史 */
    onopen: (key: string) => void;
  } = $props();

  // 兩個帳號可能都看得到同一個 org：key 要帶上帳號，不能只用 scope 名稱或 job key
  const scopes = $derived(
    snapshot.accounts.flatMap((a) =>
      a.scopes
        .filter((s) => scope === null || s.name === scope)
        .map((s) => ({ id: `${a.account_id}/${s.name}`, s })),
    ),
  );
  const waiting = $derived(scopes.flatMap(({ s }) => s.waiting.map((w) => ({ w, roster: s.roster }))));
  const runners = $derived(scopes.flatMap(({ s }) => s.runners));
  const dominant = $derived(
    dominantRepo([...runners.flatMap((r) => (r.job ? [r.job] : [])), ...waiting.map(({ w }) => w.job)]),
  );
  const maxSecs = $derived(maxElapsedSecs(runners));
  const hues = $derived(
    repoHues([...runners.flatMap((r) => (r.job ? [repoName(r.job.repo)] : [])), ...waiting.map(({ w }) => repoName(w.job.repo))]),
  );
  const groups = $derived(groupQueue(waiting));
  // 四格與下方清單用同一份（已依範圍篩選的）資料，選了單一範圍時數字才會對得上
  const sum = $derived(countVisible(runners, waiting.length));
  // 顏色只給真的需要注意的：排隊超過門檻＝琥珀（離線不再有警告色）
  const queueWarn = $derived(waiting.some(({ w }) => w.attention));
  const hasPools = $derived(splitRunners(runners).pools.length > 0);
  // 單一範圍而且有臨時機器時，排隊放在右欄（臨時機器下面）；多個範圍時放在所有範圍之後
  const queueInColumn = $derived(scopes.length === 1 && hasPools);
  const firstLoad = $derived(isLoading(snapshot, scopes.length));
</script>

<div class="runners">
  {#if firstLoad}
    <p class="empty" role="status">正在讀取 GitHub 上的 runner…第一次讀取要等十幾秒。</p>
  {:else}
    {#if dominant}
      <div class="ctx"><i class="rdot rc{hues.get(dominant) ?? 0}" aria-hidden="true"></i><b>{dominant}</b></div>
    {/if}
    <div class="stats" style:--n={hasPools ? 3 : 2} role="group" aria-label="自家 {sum.ownBusy}/{sum.ownOnline} 在忙">
      <div class="stat run"><b>{sum.ownBusy}<small>/{sum.ownOnline}</small></b><span>自家機器在忙</span></div>
      {#if hasPools}
        <div class="stat run"><b>{sum.poolBusy}</b><span>臨時機器在跑</span></div>
      {/if}
      <div class="stat" class:warn={queueWarn}><b>{sum.waiting}</b><span>排隊</span></div>
    </div>
    {#each scopes as { id, s } (id)}
      <ScopeSection scope={s} {dominant} {maxSecs} {hues} showTitle={scopes.length > 1} {limit} {onmore} {onopen}>
        {#if queueInColumn}<QueueSection {groups} total={waiting.length} {dominant} />{/if}
      </ScopeSection>
    {/each}
    {#if !queueInColumn}<QueueSection {groups} total={waiting.length} {dominant} />{/if}
  {/if}
  <StatusFooter {snapshot} {onrelogin} />
</div>
