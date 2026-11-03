<script lang="ts">
  // 用 GitHub 登入 → 代碼自動複製、自動打開 GitHub → 依 interval 輪詢 →（沒裝 App 就）自動打開安裝頁並等它裝好 → 直接進主畫面。
  // 範圍預設全選，要改到設定頁；流程的判斷在 $lib/onboarding（有單元測試）。
  import { invoke } from "@tauri-apps/api/core";
  import Selftest from "$lib/components/Selftest.svelte";
  import type { AddedAccount } from "$lib/generated/AddedAccount";
  import type { DeviceCode } from "$lib/generated/DeviceCode";
  import type { LoginStatus } from "$lib/generated/LoginStatus";
  import { finishSetup, startDeviceLogin, waitForInstall, type OnboardingDeps } from "$lib/onboarding";
  import { openUrl } from "$lib/store.svelte";

  // oncancel：已經有帳號時（新增帳號、重新登入）才給，第一次登入沒有地方可以回去
  let {
    ondone,
    oncancel,
    reauth = false,
  }: { ondone: () => void; oncancel?: () => void; reauth?: boolean } = $props();

  type Step = "start" | "code" | "install" | "pat";
  let step = $state<Step>("start");
  let code = $state<DeviceCode | null>(null);
  let copied = $state(false);
  let message = $state("");
  let login = $state("");
  let accountId = $state<number | null>(null);
  let pat = $state("");
  let busy = $state(false);
  let cancelled = false;
  let watching = false;
  /** 安裝頁已經自動打開過：按鈕改說「再打開一次」 */
  let installOpened = $state(false);

  const deps: OnboardingDeps = {
    invoke,
    openUrl,
    sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
  };

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
      const started = await startDeviceLogin(deps);
      code = started.code;
      copied = started.copied;
      if (started.openError) message = started.openError;
      step = "code";
      void poll(started.code);
    });

  const copyCode = () =>
    guarded(async () => {
      await invoke("copy_login_code");
      copied = true;
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
        await afterLogin(true);
        return;
      }
      message =
        status === "Denied" ? "你拒絕了授權。" : status === "Expired" ? "代碼已過期，請重新開始。" : "此版本未設定 GitHub App";
      step = "start";
      return;
    }
  }

  /**
   * 帳號已加入：裝到了任何帳號或 org 就直接完成；還沒裝就到安裝畫面等。
   * `openInstallPage`：GitHub App 登入時自動打開安裝頁（PAT 沒有「安裝」這回事）。
   */
  async function afterLogin(openInstallPage: boolean) {
    if (accountId === null) return;
    try {
      if ((await finishSetup(deps, accountId)) === "done") {
        ondone();
        return;
      }
      step = "install";
      if (openInstallPage) await openInstall();
    } catch (e) {
      // 帳號已經加進去了，只是讀不到可勾選的範圍：留在可重試的畫面
      message = String(e);
      step = "install";
    }
    if (openInstallPage) void watchInstall();
  }

  async function watchInstall() {
    if (watching || accountId === null) return;
    watching = true;
    const id = accountId;
    const result = await waitForInstall(deps, id, () => cancelled || step !== "install");
    watching = false;
    if (result === "done") ondone();
  }

  async function openInstall() {
    const failed = await openUrl(await invoke<string>("install_url"));
    if (failed) message = failed;
    else installOpened = true;
  }

  const refresh = () =>
    guarded(async () => {
      if (accountId !== null && (await finishSetup(deps, accountId)) === "done") ondone();
    });

  const addPat = () =>
    guarded(async () => {
      const added = await invoke<AddedAccount>("add_pat", { token: pat });
      login = added.login;
      accountId = added.account_id;
      pat = "";
      await afterLogin(false);
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
    <h1>{copied ? "貼上代碼並授權" : "輸入這組代碼"}</h1>
    <p class="user-code" aria-label="驗證碼">{code.user_code}</p>
    <p class="lead">
      {copied ? "代碼已複製。在剛打開的 GitHub 頁面貼上並按授權，這裡會自動繼續。" : "在 GitHub 頁面輸入這組代碼並按授權，這裡會自動繼續。"}
    </p>
    <button class="primary" onclick={() => openUrl(code!.verification_uri)}>再打開一次 GitHub</button>
    <button class="link" disabled={busy} onclick={copyCode}>{copied ? "再複製一次代碼" : "複製代碼"}</button>
  {:else if step === "install"}
    <h1>安裝到你的帳號或 org</h1>
    <p class="lead">已登入為 {login}。在 GitHub 的安裝頁選好帳號或 org 並按安裝，裝好這裡會自動繼續。</p>
    <button class="primary" disabled={busy} onclick={() => guarded(openInstall)}>{installOpened ? "再打開一次安裝頁" : "打開安裝頁"}</button>
    <button class="link" disabled={busy} onclick={refresh}>重新整理</button>
    <button class="link" onclick={ondone}>先跳過</button>
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
