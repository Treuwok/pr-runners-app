<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Selftest from "$lib/components/Selftest.svelte";
  import type { ScopeChoice } from "$lib/generated/ScopeChoice";
  import type { Settings } from "$lib/generated/Settings";
  import type { Snapshot } from "$lib/generated/Snapshot";
  import { applyWindowOpacity, isDesktop, saveWindowPrefs, windowPrefs, type Platform } from "$lib/store.svelte";
  import { HOURLY_TARGET_NOTE } from "$lib/view";

  let {
    snapshot,
    platform = "mac",
    onadd,
  }: { snapshot: Snapshot | null; platform?: Platform; onadd: () => void } = $props();
  /** 拖動滑桿時的即時值；放開才存 */
  let opacity = $state(windowPrefs.opacity);
  $effect(() => {
    opacity = windowPrefs.opacity;
  });

  let target = $state(2500);
  let note = $state("");
  let error = $state("");
  let scopes = $state<Record<number, ScopeChoice[]>>({});
  /** 移除帳號要按兩下：第一下只是「確定移除？」 */
  let confirming = $state<number | null>(null);

  async function run(f: () => Promise<void>) {
    error = "";
    try {
      await f();
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() =>
    run(async () => {
      target = (await invoke<Settings>("get_settings")).hourly_request_target;
    }),
  );

  const loadScopes = (accountId: number) =>
    run(async () => {
      scopes[accountId] = await invoke<ScopeChoice[]>("list_scopes", { accountId });
    });
  const saveScopes = (accountId: number) =>
    run(async () => {
      await invoke("set_scopes", {
        accountId,
        selected: scopes[accountId].filter((c) => c.selected).map((c) => c.name),
      });
    });
  const saveTarget = () =>
    run(async () => {
      // 顯示 Rust 端實際採用的值（超出 500–4,500 會被調到最近的邊界；空白或非數字當成預設值）
      const typed = Math.round(Number(target));
      const applied = await invoke<number>("set_hourly_target", { n: Number.isFinite(typed) ? typed : 2500 });
      note = applied !== typed ? `已調整為 ${applied}` : "";
      target = applied;
    });
  const remove = (accountId: number) =>
    run(async () => {
      confirming = null;
      await invoke("remove_account", { accountId });
    });
</script>

<section class="settings">
  <h2>帳號</h2>
  {#each snapshot?.accounts ?? [] as a (a.account_id)}
    <div class="acct">
      <b>{a.login}</b>
      <button class="link" onclick={() => loadScopes(a.account_id)}>勾選範圍…</button>
      {#if confirming === a.account_id}
        <button class="link danger" onclick={() => remove(a.account_id)}>確定移除？</button>
        <button class="link" onclick={() => (confirming = null)}>取消</button>
      {:else}
        <button class="link danger" onclick={() => (confirming = a.account_id)}>移除帳號</button>
      {/if}
      {#if scopes[a.account_id]}
        <ul class="choices">
          {#each scopes[a.account_id] as c (c.name)}
            <li><label><input type="checkbox" bind:checked={c.selected} onchange={() => saveScopes(a.account_id)} /> {c.name}</label></li>
          {/each}
        </ul>
      {/if}
    </div>
  {:else}
    <p class="lead">還沒有帳號。</p>
  {/each}
  <button class="link" onclick={onadd}>新增帳號</button>

  {#if isDesktop(platform)}
    <h2>視窗</h2>
    <label class="toggle">
      <input
        type="checkbox"
        checked={windowPrefs.alwaysOnTop}
        onchange={(e) => run(() => saveWindowPrefs({ alwaysOnTop: e.currentTarget.checked }))}
      />
      主視窗永遠在最上層
    </label>
    <label class="target">
      背景不透明度
      <input
        type="range"
        min="40"
        max="100"
        step="5"
        bind:value={opacity}
        oninput={() => applyWindowOpacity(opacity)}
        onchange={() => run(() => saveWindowPrefs({ opacity }))}
      />
      <span class="note">{opacity}%</span>
    </label>
    <p class="hint">只有背景會變透明，文字與進度條維持清楚。</p>
  {/if}

  <h2>更新頻率</h2>
  <label class="target">
    每小時請求目標
    <input type="number" min="500" max="4500" step="100" bind:value={target} onchange={saveTarget} />
  </label>
  {#if note}<span class="note" role="status">{note}</span>{/if}
  <p class="hint">{HOURLY_TARGET_NOTE}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <Selftest />
</section>
