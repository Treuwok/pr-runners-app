// 前端的資料來源：Rust 端每輪 emit 一份完整 Snapshot，這裡只保留最新的一份、整份重畫。
// 前端不呼叫 GitHub，也不做增量同步。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Settings } from "./generated/Settings";
import type { Snapshot } from "./generated/Snapshot";
import { type Platform, detectPlatform, isDesktop, isMobile } from "./view";

class Store {
  /** null＝還沒拿到第一份（顯示骨架畫面）；Rust 端一律回一份，所以拿到之後就不會再是 null */
  snapshot = $state<Snapshot | null>(null);

  /** Rust 端推來的事件一律接受：`generated_at` 是本機時鐘，時鐘被調回去時不能因此把畫面凍住。 */
  acceptEvent(next: Snapshot) {
    this.snapshot = next;
  }

  /** `get_snapshot` 的回應只在還沒收過任何事件時採用（事件一定比它新）。 */
  acceptInitial(next: Snapshot) {
    if (!this.snapshot) this.snapshot = next;
  }
}

export const store = new Store();

/** `?mock=1`（dev）或建置時設 `VITE_PR_RUNNERS_MOCK=1`（在模擬器上目視驗收版面用）：讀 dev-snapshot.json，不碰 Rust。 */
export const isMock = () =>
  import.meta.env.VITE_PR_RUNNERS_MOCK === "1" ||
  (import.meta.env.DEV && typeof location !== "undefined" && new URLSearchParams(location.search).has("mock"));

export async function startStore(): Promise<() => void> {
  if (isMock()) {
    const mod = await import("./dev-snapshot.json");
    store.acceptEvent(mod.default as unknown as Snapshot);
    return () => {};
  }
  // 先註冊 listener 再讀目前的 snapshot，兩者之間發生的事件不會漏掉
  const unlisten = await listen<Snapshot>("snapshot", (e) => {
    store.acceptEvent(e.payload);
  });
  // Rust 端一律回一份已對齊帳號清單的 snapshot（還沒掃完的帳號是 Loading），拿到就能離開骨架畫面
  store.acceptInitial(await invoke<Snapshot>("get_snapshot"));
  return unlisten;
}

/** 外部連結一律經 Rust 的 open_url（只接受 https://github.com/）。失敗回傳原因，成功回傳 null。 */
export async function openUrl(url: string): Promise<string | null> {
  try {
    await invoke("open_url", { url });
    return null;
  } catch (e) {
    console.error("open_url 被拒絕：", e);
    return String(e);
  }
}

/** 要求立刻完整掃描一次；「更新中」由 Rust 端在 snapshot 的 `updating` 告訴畫面。 */
export async function refreshNow(): Promise<void> {
  if (!isMock()) await invoke("refresh_now");
}

export { type Platform, isDesktop, isMobile };

/**
 * 依平台在 <html> 加 `mac`／`windows`／`android`／`ios` class，載入對應的樣式。
 * dev 的 `?mock=1&platform=android|windows|ios`：在瀏覽器裡預覽該平台的版面。
 */
export async function applyPlatformClass(): Promise<Platform> {
  let cls: Platform = "mac";
  const preview = import.meta.env.DEV && isMock() ? new URLSearchParams(location.search).get("platform") : null;
  if (preview === "android" || preview === "windows" || preview === "ios") {
    cls = preview;
  } else {
    try {
      const { platform } = await import("@tauri-apps/plugin-os");
      cls = detectPlatform(platform(), navigator.userAgent);
    } catch {
      cls = detectPlatform(null, navigator.userAgent);
    }
  }
  document.documentElement.classList.add(cls);
  return cls;
}

/** 主視窗的「永遠在最上層」與背景不透明度（只有 Mac 主視窗用得到）。 */
class WindowPrefs {
  alwaysOnTop = $state(false);
  /** 40–100（%），只影響背景，文字維持不透明 */
  opacity = $state(100);
}

export const windowPrefs = new WindowPrefs();

/** 把不透明度套到畫面上（CSS 變數）；拖動滑桿時即時預覽也用它。 */
export function applyWindowOpacity(pct: number) {
  document.documentElement.style.setProperty("--win-alpha", String(Math.min(100, Math.max(40, pct)) / 100));
}

export async function loadWindowPrefs(): Promise<void> {
  if (isMock()) return applyWindowOpacity(windowPrefs.opacity);
  const s = await invoke<Settings>("get_settings");
  windowPrefs.alwaysOnTop = s.always_on_top;
  windowPrefs.opacity = s.window_opacity;
  applyWindowOpacity(s.window_opacity);
}

/** 存進設定並立刻套用；畫面顯示 Rust 端實際採用的值（不透明度會被限制在 40–100）。 */
export async function saveWindowPrefs(next: { alwaysOnTop?: boolean; opacity?: number }): Promise<void> {
  const alwaysOnTop = next.alwaysOnTop ?? windowPrefs.alwaysOnTop;
  const opacity = Math.round(next.opacity ?? windowPrefs.opacity);
  if (isMock()) {
    windowPrefs.alwaysOnTop = alwaysOnTop;
    windowPrefs.opacity = Math.min(100, Math.max(40, opacity));
  } else {
    const s = await invoke<Settings>("set_window_prefs", { alwaysOnTop, opacity });
    windowPrefs.alwaysOnTop = s.always_on_top;
    windowPrefs.opacity = s.window_opacity;
  }
  applyWindowOpacity(windowPrefs.opacity);
}
