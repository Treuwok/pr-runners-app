//! 背景輪詢：套用 commands 排的變更 → 續期 token → tick → 存檔 → 推事件 → 睡到下一輪（或被喚醒）。
//!
//! 收集器只有這個迴圈會鎖；commands 透過 `Op` 佇列改它（見 state.rs）。

use crate::state::{Op, Shared, reconcile};
use pr_runners_core::Collector;
use pr_runners_core::auth::{RefreshOutcome, refresh_if_needed};
use pr_runners_core::persist::{AccountKind, save_snapshot, save_state};
use pr_runners_core::{Hold, TickMode, now_secs};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter};

/// 發布執行歷史：更新共享複本（`runner_history` 讀它，不碰收集器），並把還沒存成功的存到磁碟。
fn publish_runlog(shared: &Shared, c: &Collector) {
    let mut copy = shared.runlog.lock().expect("runlog");
    crate::state::publish_runlog_copy(
        &shared.data_dir,
        &mut copy,
        c.runlog(),
        &shared.runlog_dirty,
    );
}

/// 把 commands 排的變更套用到收集器。
async fn apply_ops(shared: &Shared) {
    let ops: Vec<Op> = std::mem::take(&mut *shared.ops.lock().expect("ops"));
    if ops.is_empty() {
        return;
    }
    let mut c = shared.collector.lock().await;
    for op in ops {
        match op {
            Op::AddAccount { meta, client } => c.add_account(meta, *client),
            Op::SetScopes {
                account_id,
                selected,
            } => c.set_scopes(account_id, selected),
            Op::RemoveAccount { account_id } => {
                c.remove_account(account_id);
                // 命令端已刪過一次；這裡再刪一次，蓋住「刪除當下剛好有一個續期在途」的情況
                let _ = shared.secret_store().delete(account_id);
            }
            Op::SetHourlyTarget(n) => c.set_hourly_target(n),
            Op::Backfill {
                account_id,
                scope,
                runner_name,
                runner_key,
                reply,
            } => {
                let report = c
                    .backfill(account_id, &scope, &runner_name, &runner_key)
                    .await;
                // 先發布再回覆：指令收到回覆後馬上會重讀歷史，必須讀到補查的結果
                publish_runlog(shared, &c);
                let _ = reply.send(report);
            }
        }
    }
}

/// 每一輪開始前，替 GitHub App 帳號檢查 token 要不要續期。
/// 續期成功會讓舊 access token 與舊 refresh token 同時失效，所以先寫金鑰庫（失敗時由 `RetryingStore`
/// 留在記憶體重試）、再換 HTTP client 的 token，之後的請求才開始。PAT 不續期。
async fn refresh_tokens(shared: &Shared) {
    let Some(flow) = shared.flow.as_ref() else {
        return;
    };
    let accounts = shared.accounts.lock().expect("accounts").clone();
    for meta in accounts.iter().filter(|a| a.kind == AccountKind::GitHubApp) {
        // 與「移除帳號」互斥：續期（含寫金鑰庫）進行中時，移除要等它結束；移除先發生就直接跳過
        let _lifecycle = shared.lifecycle.lock().await;
        if !shared
            .accounts
            .lock()
            .expect("accounts")
            .iter()
            .any(|a| a.account_id == meta.account_id)
        {
            continue;
        }
        let outcome =
            refresh_if_needed(flow, shared.secret_store(), meta.account_id, now_secs()).await;
        let mut c = shared.collector.lock().await;
        match outcome {
            RefreshOutcome::NotNeeded => c.hold(meta.account_id, None),
            RefreshOutcome::Refreshed { access_token } => {
                c.set_token(meta.account_id, access_token);
                c.hold(meta.account_id, None);
            }
            RefreshOutcome::NeedsReauth => c.hold(meta.account_id, Some(Hold::NeedsReauth)),
            RefreshOutcome::Offline => c.hold(meta.account_id, Some(Hold::Offline)),
        }
    }
}

pub async fn run_loop(app: AppHandle, shared: Arc<Shared>) {
    let mut first = true;
    loop {
        // Android 切到背景：不開始新的一輪，等回前景（進行中的掃描已被 halt 旗標中止）
        if shared.halt.load(Ordering::SeqCst) {
            shared.wake.notified().await;
            continue;
        }
        apply_ops(&shared).await;
        refresh_tokens(&shared).await;

        let mode = if first || shared.force_full.swap(false, Ordering::SeqCst) {
            TickMode::Full
        } else {
            TickMode::Incremental
        };
        first = false;

        let (mut snap, delay) = {
            let mut c = shared.collector.lock().await;
            if shared.resume_pending.swap(false, Ordering::SeqCst) {
                c.resume();
            }
            let snap = c.tick(mode).await;
            let delay = c.next_delay(&snap);
            // 存檔失敗不該讓輪詢停下來；下一輪會再存
            let _ = save_state(&shared.data_dir, &c.states());
            publish_runlog(&shared, &c);
            (snap, delay)
        };
        // 被中止的一輪（背景）不發布：它是半套資料
        if shared.halt.load(Ordering::SeqCst) {
            continue;
        }
        // 發布整段（對齊帳號清單 → 記住 → 存檔 → 推事件）都在 `latest` 鎖裡：
        // 掃描期間使用者可能剛移除帳號或改勾選，命令端的「立刻修剪並推事件」也拿同一把鎖，
        // 所以最後被畫面收到的一定是用「最新帳號清單」對齊過的那份，不會有舊的那份蓋回去。
        {
            let mut latest = shared.latest.lock().expect("latest");
            let accounts = shared.accounts.lock().expect("accounts").clone();
            reconcile(&mut snap, &accounts);
            let _ = save_snapshot(&shared.data_dir, &snap);
            *latest = Some(snap.clone());
            let _ = app.emit("snapshot", &snap);
        }
        #[cfg(desktop)]
        crate::tray::update(&app, &snap.summary);

        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = shared.wake.notified() => {}
        }
    }
}
