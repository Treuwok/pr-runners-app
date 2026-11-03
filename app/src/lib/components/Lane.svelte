<script lang="ts">
  // 一台 runner 一列：名稱、正在跑哪個 job（GitHub 原名）、一條細的已跑長度、時間。
  import type { RunnerView } from "$lib/generated/RunnerView";
    import { dotFor, eta, etaText, fillPercent, formatElapsed, laneLabel } from "$lib/view";

  let {
    runner,
    dominant,
    maxSecs,
    hues,
    onopen,
  }: {
    runner: RunnerView;
    dominant: string | null;
    maxSecs: number;
    hues: Map<string, number>;
    /** 點一下打開這台 runner 的執行歷史 */
    onopen: (key: string) => void;
  } = $props();
  const label = $derived(laneLabel(runner, dominant));
  const busy = $derived(runner.state === "Busy");
  const est = $derived(eta(runner.elapsed, runner.job?.typical_secs));
  // 有預估：條長＝已跑 ÷ 平常要跑多久；沒有預估：灰色條，只跟其他泳道比長短
  const pct = $derived(est.kind === "ontrack" ? est.pct : est.kind === "over" ? 100 : fillPercent(runner.elapsed, maxSecs));
</script>

<div class="lane-wrap">
  <button class="lane" class:bad={runner.attention} title="查看執行歷史" onclick={() => onopen(runner.key)}>
    <span class="who">
      <span class="dot {dotFor(runner)}" class:alert={runner.attention} aria-hidden="true"></span>
      <span class="nm">{runner.name}</span>
    </span>
    <span class="job" class:quiet={!busy}>
      {#if label.prefix}<span class="rp rc{hues.get(label.prefix) ?? 0}">{label.prefix}</span>{/if}{label.text}
    </span>
    {#if busy}
      <span class="meter {est.kind}" aria-hidden="true"><span class="fill" style:width="{pct}%"></span></span>
    {:else}
      <span aria-hidden="true"></span>
    {/if}
    <span class="t" class:over={est.kind === "over"}>
      {#if est.kind === "none"}
        {formatElapsed(runner.elapsed)}
      {:else}
        {etaText(est)}<small>已跑 {formatElapsed(runner.elapsed)} · 平常 {est.typicalMin} 分</small>
      {/if}
    </span>
  </button>
</div>
