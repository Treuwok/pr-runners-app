<script lang="ts">
  // 一台 runner 的執行歷史：頂端是 runner 資訊，下面依結果篩選的紀錄，最下面可以向 GitHub 補查更早的紀錄。
  import { invoke } from "@tauri-apps/api/core";
  import type { BackfillReport } from "$lib/generated/BackfillReport";
  import type { JobRecord } from "$lib/generated/JobRecord";
  import type { Snapshot } from "$lib/generated/Snapshot";
  import { isMock, openUrl } from "$lib/store.svelte";
  import {
    backfillNote,
    countRecords,
    dotFor,
    filterRecords,
    findRunner,
    laneText,
    outcomeLabel,
    recordDuration,
    recordWhen,
    repoName,
    type HistoryFilter,
  } from "$lib/view";

  let { snapshot, runnerKey, onback }: { snapshot: Snapshot; runnerKey: string; onback: () => void } = $props();

  const found = $derived(findRunner(snapshot, runnerKey));
  let records = $state<JobRecord[]>([]);
  let filter = $state<HistoryFilter>("all");
  let busy = $state(false);
  let note = $state("");
  let error = $state("");

  async function load() {
    if (isMock()) {
      const mod = await import("$lib/dev-runlog.json");
      records = ((mod.default as Record<string, JobRecord[]>)[runnerKey] ?? []) as JobRecord[];
      return;
    }
    records = await invoke<JobRecord[]>("runner_history", { runnerKey });
  }

  // 每份新的 snapshot 都重讀一次（剛結束的 job 會在那時拿到結果）
  $effect(() => {
    void snapshot.generated_at;
    void runnerKey;
    load().catch((e) => (error = String(e)));
  });

  async function backfill() {
    if (!found || busy) return;
    busy = true;
    error = "";
    note = "";
    try {
      const r = isMock()
        ? { repos_checked: 2, runs_checked: 16, added: 0, partial: false }
        : await invoke<BackfillReport>("backfill_runner_history", {
            accountId: found.accountId,
            scope: found.scope,
            runnerName: found.runner.name,
            runnerKey,
          });
      note = backfillNote(r);
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  const counts = $derived(countRecords(records));
  const shown = $derived(filterRecords(records, filter));
  const filters: [HistoryFilter, string][] = [
    ["all", "全部"],
    ["ok", "成功"],
    ["bad", "失敗"],
    ["other", "其他"],
  ];
</script>

<section class="history">
  <button class="back" onclick={onback}>‹ Runners</button>

  {#if found}
    {@const r = found.runner}
    <header class="hhead">
      <h2><span class="dot {dotFor(r)}" class:alert={r.attention} aria-hidden="true"></span>{r.name}</h2>
      <p class="hstate">{laneText(r, null)}</p>
      <p class="hmeta">
        {r.os || "OS 不明"} · {r.group ?? "group 不明"} · {found.scope}
        {#each r.labels as l}<span class="chip">{l}</span>{/each}
      </p>
      {#if r.job}
        <p class="hmeta">
          目前：{repoName(r.job.repo)} · {r.job.workflow} → {r.job.job} · {r.job.pr_tag}
          <button class="link" onclick={() => openUrl(r.job!.url)}>在 GitHub 上打開</button>
        </p>
      {/if}
    </header>
  {:else}
    <header class="hhead"><h2>這台 runner 目前不在畫面上</h2><p class="hstate">以下是之前記下的紀錄。</p></header>
  {/if}

  <div class="tabs hfilter" role="group" aria-label="依結果篩選">
    {#each filters as [id, label] (id)}
      <button class:on={filter === id} aria-pressed={filter === id} onclick={() => (filter = id)}>{label} {counts[id]}</button>
    {/each}
  </div>

  {#if shown.length > 0}
    <div class="card">
      {#each shown as rec (rec.job_id)}
        {@const o = outcomeLabel(rec.outcome)}
        <button class="hrow" onclick={() => rec.url && openUrl(rec.url)}>
          <span class="hwhen">{recordWhen(rec.started_at ?? rec.completed_at)}</span>
          <span class="hjob">
            <span class="hname">{rec.job}</span>
            <small>{repoName(rec.repo)} · {rec.workflow} · {rec.pr_tag}{rec.source === "Backfill" ? " · 補查" : ""}</small>
          </span>
          <span class="badge {o.tone}"><i aria-hidden="true"></i>{o.text}</span>
          <span class="hdur">{recordDuration(rec)}</span>
        </button>
      {/each}
    </div>
  {:else if records.length === 0}
    <p class="empty">還沒有紀錄。App 開著時會自動記下這台跑過的 job；也可以按下面的按鈕向 GitHub 補查。</p>
  {:else}
    <p class="empty">這個分類沒有紀錄。</p>
  {/if}

  <div class="hmore">
    <button class="primary" disabled={busy || !found} onclick={backfill}>
      {busy ? "向 GitHub 補查中…" : "載入更早的紀錄"}
    </button>
    <p class="hint">向 GitHub 查最近 7 天有推送的 repo（最多 6 個、每個最近 8 次執行），大約花 50 次請求。</p>
    {#if note}<p class="note" role="status">{note}</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
</section>
