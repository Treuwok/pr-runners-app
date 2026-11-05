<script lang="ts">
  // Mac 小視窗、Mac 主視窗與 Android 的 Runners 頁共用同一個內容：
  // 頂端一行（repo＋計數）→ 每個 scope 一段泳道 → 排隊（只列一次）→ 底部狀態。
  import type { Snapshot } from "$lib/generated/Snapshot";
  import QueueSection from "$lib/components/QueueSection.svelte";
  import ScopeSection from "$lib/components/ScopeSection.svelte";
  import StatusFooter from "$lib/components/StatusFooter.svelte";
  import { countVisible, dominantRepo, groupQueue, maxElapsedSecs, repoHues, repoName } from "$lib/view";

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
  const ownOffline = $derived(runners.filter((r) => !r.pool && typeof r.state === "object").length);
</script>

{#if dominant}
  <div class="ctx"><i class="rdot rc{hues.get(dominant) ?? 0}" aria-hidden="true"></i><b>{dominant}</b></div>
{/if}
<div class="stats" role="group" aria-label={`${sum.ownBusy}/${sum.ownOnline} 自家在忙`}>
  <div class="stat run"><b>{sum.ownBusy + sum.poolBusy}</b><span>忙碌</span></div>
  <div class="stat"><b>{sum.ownOnline - sum.ownBusy}</b><span>閒置</span></div>
  <div class="stat" class:warn={queueWarn}><b>{sum.waiting}</b><span>排隊</span></div>
  <div class="stat"><b>{ownOffline}</b><span>離線</span></div>
</div>
{#each scopes as { id, s } (id)}
  <ScopeSection scope={s} {dominant} {maxSecs} {hues} showTitle={scopes.length > 1} {limit} {onmore} {onopen} />
{/each}
<QueueSection {groups} total={waiting.length} {dominant} />
<StatusFooter {snapshot} {onrelogin} />
