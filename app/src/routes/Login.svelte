<script lang="ts">
  // spec §4：用 GitHub 登入 → 8 碼代碼＋「打開 GitHub」→ 依 interval 輪詢 → （沒裝 App 就）安裝 → 勾選範圍。
  import { invoke } from "@tauri-apps/api/core";
  import Selftest from "$lib/components/Selftest.svelte";
  import type { AddedAccount } from "$lib/generated/AddedAccount";
  import type { DeviceCode } from "$lib/generated/DeviceCode";
  import type { LoginStatus } from "$lib/generated/LoginStatus";
  import type { ScopeChoice } from "$lib/generated/ScopeChoice";
  import { openUrl } from "$lib/store.svelte";
  import { defaultSelected } from "$lib/view";

  // oncancel：已經有帳號時（新增帳號、重新登入）才給，第一次登入沒有地方可以回去
  let {
    ondone,
    oncancel,
    reauth = false,
  }: { ondone: () => void; oncancel?: () => void; reauth?: boolean } = $props();

  type Step = "start" | "code" | "scopes" | "install" | "pat";
  let step = $state<Step>("start");
  let code = $state<DeviceCode | null>(null);
  let message = $state("");
  let login = $state("");
  let accountId = $state<number | null>(null);
  let choices = $state<ScopeChoice[]>([]);
  let pat = $state("");
  let busy = $state(false);
  let cancelled = false;

  /** 任何一步失敗都留在畫面上顯示原因，不讓使用者卡在沒有反應的畫面。 */
  async function guarded(f: () => Promise<void>) {
    if (busy) return;
    busy = true;
    message = "";
    try {
      await f();
    } catch (e) {
      message = String(e);
    } finally {
      busy = false;
    }
  }

  const start = () =>
    guarded(async () => {
      code = await invoke<DeviceCode>("login_start");
      step = "code";
      void poll(code);
    });

  async function poll(c: DeviceCode) {
    let interval = c.interval;
    while (!cancelled) {
      await new Promise((r) => setTimeout(r, interval * 1000));
      if (cancelled) return;
      let status: LoginStatus;
      try {
        status = await invoke<LoginStatus>("login_poll", { deviceCode: c.device_code });
      } catch (e) {
        // 授權可能已完成、只是建立帳號的後半段暫時失敗（Rust 端會留著 token）：同一組代碼再試一次
        message = String(e);
        if (Date.now() / 1000 > c.expires_at) {
          step = "start";
          return;
        }
        continue;
      }
      message = "";
      if (status === "Pending") continue;
      if (typeof status === "object" && "SlowDown" in status) {
        interval = status.SlowDown.new_interval; // 之後改用增加後的間隔
        continue;
      }
      if (typeof status === "object" && "Done" in status) {
        login = status.Done.login;
        accountId = status.Done.account_id;
        try {
          await loadChoices();
        } catch (e) {
          // 帳號已經加進去了，只是讀不到可勾選的範圍：留在可重試的畫面
          message = String(e);
          step = "install";
        }
        return;
      }
      message =
        status === "Denied" ? "你拒絕了授權。" : status === "Expired" ? "代碼已過期，請重新開始。" : "此版本未設定 GitHub App";
      step = "start";
      return;
    }
  }

  async function loadChoices() {
    if (accountId === null) return;
    choices = defaultSelected(await invoke<ScopeChoice[]>("list_scopes", { accountId }));
    step = choices.length === 0 ? "install" : "scopes";
  }

  const refresh = () => guarded(loadChoices);

  const addPat = () =>
    guarded(async () => {
      const added = await invoke<AddedAccount>("add_pat", { token: pat });
      login = added.login;
      accountId = added.account_id;
      pat = "";
      try {
        await loadChoices();
      } catch (e) {
        // 帳號已經加進去了，只是讀不到可勾選的範圍：和裝置登入一樣留在可重試的畫面
        message = String(e);
        step = "install";
      }
    });

  const finish = () =>
    guarded(async () => {
      if (accountId !== null) {
        await invoke("set_scopes", {
          accountId,
          selected: choices.filter((c) => c.selected).map((c) => c.name),
        });
      }
      ondone();
    });

  const install = () =>
    guarded(async () => {
      const failed = await openUrl(await invoke<string>("install_url"));
      if (failed) message = failed;
    });

  $effect(() => () => {
    cancelled = true;
  });
</script>

<section class="login">
  {#if oncancel && (step === "start" || step === "pat" || step === "code")}
    <button class="link" onclick={oncancel}>← 返回</button>
  {/if}
  {#if step === "start"}
    <h1>{reauth ? "重新登入" : "pr-runners"}</h1>
    <p class="lead">看你的 self-hosted runner 現在在忙什麼、有沒有東西卡住。唯讀，不會動你的 repo。</p>
    <button class="primary" disabled={busy} onclick={start}>用 GitHub 登入</button>
    <button class="link" onclick={() => (step = "pat")}>改用 Personal Access Token</button>
  {:else if step === "code" && code}
    <h1>輸入這組代碼</h1>
    <p class="user-code" aria-label="驗證碼">{code.user_code}</p>
    <button class="primary" onclick={() => openUrl(code!.verification_uri)}>打開 GitHub</button>
    <p class="lead">在 GitHub 頁面貼上代碼並授權，這裡會自動繼續。</p>
  {:else if step === "install"}
    <h1>安裝到你的帳號或 org</h1>
    <p class="lead">已登入為 {login}，但這個 App 還沒裝到任何帳號或 org。</p>
    <button class="primary" disabled={busy} onclick={install}>安裝到你的帳號或 org</button>
    <button class="link" disabled={busy} onclick={refresh}>重新整理</button>
    <button class="link" onclick={ondone}>先跳過</button>
  {:else if step === "scopes"}
    <h1>要看哪些範圍？</h1>
    <ul class="choices">
      {#each choices as c (c.name)}
        <li><label><input type="checkbox" bind:checked={c.selected} /> {c.name}</label></li>
      {/each}
    </ul>
    <button class="primary" disabled={busy} onclick={finish}>完成</button>
  {:else if step === "pat"}
    <h1>Personal Access Token</h1>
    <p class="lead">貼上唯讀的 fine-grained token（Actions、Administration、Metadata、Pull requests 的 Read）。</p>
    <input
      class="pat"
      type="password"
      autocomplete="off"
      aria-label="Personal Access Token"
      placeholder="github_pat_…"
      bind:value={pat}
    />
    <button class="primary" disabled={busy || !pat.trim()} onclick={addPat}>加入</button>
    <button class="link" onclick={() => (step = "start")}>返回</button>
  {/if}
  {#if message}<p class="error" role="alert">{message}</p>{/if}
  <Selftest />
</section>
