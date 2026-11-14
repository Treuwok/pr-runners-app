// 畫面上「讀取中／更新中」的判斷：資料來自 Rust 端的 snapshot（帳號狀態 Loading、`updating`），前端不自己猜。
import type { Snapshot } from "./generated/Snapshot";

/** 有一次要求的掃描還沒跑完時顯示的字（底部狀態列與手機版的下拉區共用）。 */
export const UPDATING_NOTE = "更新中…";

/**
 * 畫面上還沒有任何範圍可以顯示，而且資料還在路上（有帳號還沒掃過、或剛要求的掃描還沒跑完）：
 * 這時要顯示「讀取中」，不是一排 0 或「沒有 job」——那看起來像「什麼都沒有」。
 */
export function isLoading(snapshot: Snapshot, visibleScopes: number): boolean {
  return visibleScopes === 0 && (snapshot.updating || snapshot.accounts.some((a) => a.status === "Loading"));
}
