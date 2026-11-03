// 登入後的設定流程：把「複製代碼、打開 GitHub、等待安裝、勾選範圍」這些原本要使用者自己按的步驟自動化。
// 和 Tauri 的接點（invoke、開網址、計時）由呼叫端注入，所以可以在 vitest 裡直接測。
import type { DeviceCode } from "./generated/DeviceCode";
import type { ScopeChoice } from "./generated/ScopeChoice";
import { defaultSelected } from "./view";

export type OnboardingDeps = {
  invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  /** 成功回 null，失敗回原因（同 store 的 openUrl） */
  openUrl: (url: string) => Promise<string | null>;
  sleep: (ms: number) => Promise<void>;
};

/** 等待安裝時多久檢查一次。只在安裝畫面上才會跑，一分鐘 12 次請求。 */
export const INSTALL_CHECK_MS = 5_000;

export type StartedLogin = {
  code: DeviceCode;
  /** 代碼已在剪貼簿：畫面改說「貼上」 */
  copied: boolean;
  /** 瀏覽器打不開的原因；null 代表已打開 */
  openError: string | null;
};

/** 取得代碼 → 放進剪貼簿 → 打開 GitHub 的輸入頁。複製或開網址失敗都不算登入失敗，畫面照樣顯示代碼。 */
export async function startDeviceLogin(deps: OnboardingDeps): Promise<StartedLogin> {
  const code = await deps.invoke<DeviceCode>("login_start");
  const copied = await deps.invoke("copy_login_code").then(
    () => true,
    () => false,
  );
  const openError = await deps.openUrl(code.verification_uri);
  return { code, copied, openError };
}

/**
 * 帳號已經加進去之後：裝到了任何帳號或 org，就用預設勾選直接完成；一個都沒有就回報需要安裝。
 * 預設勾選沿用 `defaultSelected`（沒有任何勾選時全選，重新登入時保留原本的勾選）。要改範圍到設定頁。
 */
export async function finishSetup(deps: OnboardingDeps, accountId: number): Promise<"done" | "install"> {
  const choices = defaultSelected(await deps.invoke<ScopeChoice[]>("list_scopes", { accountId }));
  if (choices.length === 0) return "install";
  await deps.invoke("set_scopes", {
    accountId,
    selected: choices.filter((c) => c.selected).map((c) => c.name),
  });
  return "done";
}

/** 使用者在 GitHub 安裝的期間定時檢查，裝好就自動完成，不用回來按「重新整理」。查詢暫時失敗就下一輪再試。 */
export async function waitForInstall(
  deps: OnboardingDeps,
  accountId: number,
  cancelled: () => boolean,
  intervalMs = INSTALL_CHECK_MS,
): Promise<"done" | "cancelled"> {
  // 先等再查：呼叫端剛查過一次「還沒安裝」，馬上再查只是多花一次請求
  while (!cancelled()) {
    await deps.sleep(intervalMs);
    if (cancelled()) break;
    try {
      if ((await finishSetup(deps, accountId)) === "done") return "done";
    } catch {
      // 網路暫時不通、GitHub 暫時回錯誤：等下一輪
    }
  }
  return "cancelled";
}
