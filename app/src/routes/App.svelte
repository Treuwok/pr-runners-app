<script lang="ts">
  // 主視窗（Mac）與整個 App（Android）。Mac 主視窗頂部有 scope 篩選；Android 底部三頁導覽。
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { applyPlatformClass, isDesktop, isMobile, isMock, type Platform, loadWindowPrefs, refreshNow, saveWindowPrefs, startStore, store, windowPrefs } from "$lib/store.svelte";
  import RunnerHistory from "$lib/components/RunnerHistory.svelte";
  import Jobs from "./Jobs.svelte";
  import Login from "./Login.svelte";
  import RunnersView from "./RunnersView.svelte";
  import Settings from "./Settings.svelte";
  import { UPDATING_NOTE } from "$lib/loading";

  type Tab = "runners" | "jobs" | "settings";
  let tab = $state<Tab>("runners");
  let adding = $state(false);
  let reauth = $state(false);
  /** 一旦需要登入就留著登入畫面直到完成：帳號一加進去，下一份 snapshot 就有帳號了，
   *  不能因此把還沒走完的「安裝 App／勾選範圍」步驟收掉。 */
  let onboarding = $state(false);
  let scope = $state<string | null>(null);
  /** 正在看哪台 runner 的執行歷史；null＝Runners 清單 */
  let historyKey = $state<string | null>(null);
  let platform = $state<Platform>("mac");

  onMount(() => {
    let stop = () => {};
    // 選單列小視窗打開主視窗：視窗只是被藏起來，分頁與篩選都還在，所以每次都明確指定目的地
    const gotoSettings = listen("goto-settings", () => {
      tab = "settings";
      adding = false;
    });
    // 小視窗點某台 runner：切到 Runners 並打開那台的歷史
    const gotoRunner = listen<string>("goto-runner", (e) => {
      tab = "runners";
      historyKey = e.payload;
    });
    const gotoRunners = listen("goto-runners", () => {
      // 只動分頁與篩選：正在進行的登入流程不能因為「打開主視窗」被中斷
      tab = "runners";
      scope = null;
      historyKey = null;
    });
    // 行動版（Android、iOS）：切到背景就暫停輪詢，回前景立刻刷新（Mac 的背景持續輪詢，打開視窗不算「回到前景」）
    const onVisibility = () => {
      if (!isMobile(platform) || isMock()) return;
      void invoke(document.hidden ? "app_paused" : "app_resumed");
    };
    document.addEventListener("visibilitychange", onVisibility);
    applyPlatformClass().then((p) => {
      platform = p;
      if (isDesktop(p)) void loadWindowPrefs().catch((e) => console.error("讀不到視窗設定：", e));
    });
    startStore().then((f) => (stop = f));
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
      gotoSettings.then((f) => f());
      gotoRunners.then((f) => f());
      gotoRunner.then((f) => f());
    };
  });

  // 下拉重新整理（Android）：在頁面最上面往下拉超過門檻就觸發一次完整掃描
  let pull = $state(0);
  let startY: number | null = null;
  const PULL_THRESHOLD = 80;
  function touchStart(e: TouchEvent) {
    startY = window.scrollY <= 0 ? e.touches[0].clientY : null;
  }
  function touchMove(e: TouchEvent) {
    if (startY !== null) pull = Math.max(0, Math.min(PULL_THRESHOLD * 1.5, e.touches[0].clientY - startY));
  }
  function touchEnd() {
    if (pull >= PULL_THRESHOLD) void refreshNow();
    pull = 0;
    startY = null;
  }

  const snap = $derived(store.snapshot);
  const scopeNames = $derived([...new Set((snap?.accounts ?? []).flatMap((a) => a.scopes.map((s) => s.name)))]);
  const needLogin = $derived(snap !== null && snap.accounts.length === 0);
  $effect(() => {
    if (needLogin) onboarding = true;
  });
  // 取消勾選或移除帳號後，篩選的範圍可能已經不存在：回到「全部」
  $effect(() => {
    if (scope !== null && !scopeNames.includes(scope)) scope = null;
  });
</script>

<!-- 底部導覽的圖示：同一套 24px、1.8 線寬的線條圖；選中時 .f 的部分填滿（Runners＝四格 runner，和 App 圖示同一個語彙） -->
{#snippet tabIcon(id: string)}
  <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    {#if id === "runners"}
      <rect class="f" x="3.5" y="3.5" width="7" height="7" rx="2" />
      <rect x="13.5" y="3.5" width="7" height="7" rx="2" />
      <rect x="3.5" y="13.5" width="7" height="7" rx="2" />
      <rect class="f" x="13.5" y="13.5" width="7" height="7" rx="2" />
    {:else if id === "jobs"}
      <path d="M9.5 6.5h11M9.5 12h11M9.5 17.5h11" />
      <circle class="f" cx="4.5" cy="6.5" r="1.4" />
      <circle class="f" cx="4.5" cy="12" r="1.4" />
      <circle class="f" cx="4.5" cy="17.5" r="1.4" />
    {:else}
      <path d="M3.5 7h9M18.5 7h2M3.5 17h3M12.5 17h8" />
      <circle class="f" cx="15.5" cy="7" r="2.4" />
      <circle class="f" cx="9.5" cy="17" r="2.4" />
    {/if}
  </svg>
{/snippet}

<div class="app {platform}">
  {#if isMock()}<p class="mock-banner" role="status">示意資料（mock）— 不是真實的 runner 狀態</p>{/if}
  {#if !snap}
    <div class="skeleton" aria-busy="true" aria-label="載入中">
      {#each [0, 1, 2, 3] as i (i)}<div class="sk-row"></div>{/each}
    </div>
  {:else if onboarding || adding || reauth}
    <Login
      {reauth}
      oncancel={adding || reauth ? () => ((adding = false), (reauth = false)) : undefined}
      ondone={() => {
        onboarding = false;
        adding = false;
        reauth = false;
      }}
    />
  {:else if snap}
    <header class="appbar">
      <h1>Runners</h1>
      {#if isDesktop(platform)}
        <nav class="tabs" aria-label="頁面">
          {#each [["runners", "Runners"], ["jobs", "Jobs"], ["settings", "設定"]] as [id, label] (id)}
            <button class:on={tab === id} aria-current={tab === id ? "page" : undefined} onclick={() => ((tab = id as Tab), (historyKey = null))}>{label}</button>
          {/each}
        </nav>
        <button
          class="pin"
          class:on={windowPrefs.alwaysOnTop}
          aria-pressed={windowPrefs.alwaysOnTop}
          title={windowPrefs.alwaysOnTop ? "取消永遠在最上層" : "永遠在最上層"}
          aria-label="永遠在最上層"
          onclick={() => void saveWindowPrefs({ alwaysOnTop: !windowPrefs.alwaysOnTop }).catch((e) => console.error(e))}
        >
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <path d="M5.5 1.5h5l-.8 4.2 2.8 2.8v1H8.6V15l-.6.5-.6-.5V9.5H3.5v-1l2.8-2.8z" fill={windowPrefs.alwaysOnTop ? "currentColor" : "none"} stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
          </svg>
        </button>
        {#if tab === "runners" && scopeNames.length > 1}
          <select bind:value={scope} aria-label="範圍">
            <option value={null}>全部</option>
            {#each scopeNames as n (n)}<option value={n}>{n}</option>{/each}
          </select>
        {/if}
      {/if}
    </header>
    <main
      ontouchstart={isMobile(platform) ? touchStart : undefined}
      ontouchmove={isMobile(platform) ? touchMove : undefined}
      ontouchend={isMobile(platform) ? touchEnd : undefined}
    >
      {#if pull > 0}
        <div class="pull" style:height="{pull / 2}px" aria-hidden="true">
          {pull >= PULL_THRESHOLD ? "放開重新整理" : "下拉重新整理"}
        </div>
      {:else if snap.updating && isMobile(platform)}
        <!-- 放開之後（或剛回到前景）一輪掃描還要好幾秒：留著提示，直到那一輪跑完 -->
        <div class="pull" style:height="{PULL_THRESHOLD / 2}px" aria-hidden="true">{UPDATING_NOTE}</div>
      {/if}
      {#if tab === "runners"}
        {#if historyKey}
          <RunnerHistory snapshot={snap} runnerKey={historyKey} onback={() => (historyKey = null)} />
        {:else}
          <RunnersView snapshot={snap} {scope} onrelogin={() => (reauth = true)} onopen={(k) => (historyKey = k)} />
        {/if}
      {:else if tab === "jobs"}
        <Jobs snapshot={snap} />
      {:else}
        <Settings snapshot={snap} {platform} onadd={() => (adding = true)} />
      {/if}
    </main>
    {#if isMobile(platform)}
      <nav class="bottom" aria-label="頁面">
        {#each [["runners", "Runners"], ["jobs", "Jobs"], ["settings", "設定"]] as [id, label] (id)}
          <button class:on={tab === id} aria-current={tab === id ? "page" : undefined} onclick={() => ((tab = id as Tab), (historyKey = null))}>
            <span class="ico">{@render tabIcon(id)}</span>{label}
          </button>
        {/each}
      </nav>
    {/if}
  {/if}
</div>
