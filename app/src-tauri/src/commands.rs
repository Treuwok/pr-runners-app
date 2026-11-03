//! 前端可呼叫的 commands。邏輯盡量放在不依賴 `AppHandle` 的純函式，方便單元測試。

use crate::state::{Op, Shared, reconcile};
use pr_runners_core::api::{self, ScopeKind};
use pr_runners_core::auth::{DeviceCode, DeviceFlow, PollOutcome, TokenSet};
use pr_runners_core::persist::{
    self, AccountKind, AccountMeta, Settings, clamp_hourly_target, clamp_window_opacity,
    load_settings, save_settings,
};
use pr_runners_core::runlog::{BackfillReport, JobRecord};
use pr_runners_core::view::Snapshot;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::{Emitter, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;
use ts_rs::TS;

pub type AppState<'a> = State<'a, Arc<Shared>>;

const NOT_CONFIGURED: &str = "此版本未設定 GitHub App";

/// 登入流程需要 GitHub App 的 client ID（建置時注入）；沒有就回「未設定」。
pub fn require_flow(flow: Option<&DeviceFlow>) -> Result<&DeviceFlow, LoginStatus> {
    flow.ok_or(LoginStatus::NotConfigured)
}

// ───────────────────────── 純函式（有單元測試） ─────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub enum LoginStatus {
    Pending,
    /// 之後改用這個間隔（秒）輪詢，不能丟成一般 Pending
    SlowDown {
        new_interval: u64,
    },
    /// 登入完成。`account_id` 讓前端接著呼叫 `list_scopes`／`set_scopes`，不必猜新帳號是哪一個。
    Done {
        login: String,
        account_id: u64,
    },
    Denied,
    Expired,
    /// 建置時沒有設定 GitHub App 的 client ID
    NotConfigured,
}

/// `finished`：授權完成後已經取得的帳號（只有 `Done` 會用到）。
pub fn login_status_from(outcome: &PollOutcome, finished: Option<&AddedAccount>) -> LoginStatus {
    match outcome {
        PollOutcome::Pending => LoginStatus::Pending,
        PollOutcome::SlowDown { new_interval } => LoginStatus::SlowDown {
            new_interval: *new_interval,
        },
        PollOutcome::Done(_) => match finished {
            Some(a) => LoginStatus::Done {
                login: a.login.clone(),
                account_id: a.account_id,
            },
            // 授權完成但沒能建立帳號是 bug（finish_login 失敗會直接回 Err），不要假裝成功
            None => LoginStatus::Pending,
        },
        PollOutcome::Denied => LoginStatus::Denied,
        PollOutcome::Expired => LoginStatus::Expired,
    }
}

/// 剛加入的帳號。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct AddedAccount {
    pub account_id: u64,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ScopeChoice {
    pub name: String,
    pub kind: ScopeKind,
    pub selected: bool,
}

pub fn scope_choices(
    found: &[(String, ScopeKind)],
    selected: &BTreeSet<String>,
) -> Vec<ScopeChoice> {
    let mut out: Vec<ScopeChoice> = found
        .iter()
        .map(|(name, kind)| ScopeChoice {
            name: name.clone(),
            kind: *kind,
            selected: selected.contains(name),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// 只有 `https://github.com/…` 可以由 App 開啟。
pub fn allowed_url(u: &str) -> bool {
    if !u.starts_with("https://github.com/") {
        return false;
    }
    match url::Url::parse(u) {
        Ok(parsed) => {
            parsed.scheme() == "https"
                && parsed.host_str() == Some("github.com")
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.port().is_none()
        }
        Err(_) => false,
    }
}

/// 安裝頁網址。slug 取自建置時的 `PR_RUNNERS_GITHUB_APP_SLUG`，預設 `pr-runners`。
pub fn install_url_for(slug: Option<&str>) -> String {
    format!(
        "https://github.com/apps/{}/installations/new",
        slug.filter(|s| !s.is_empty()).unwrap_or("pr-runners")
    )
}

// ───────────────────────── commands ─────────────────────────

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn mirror(state: &Shared) -> Vec<AccountMeta> {
    state.accounts.lock().expect("accounts").clone()
}

#[tauri::command]
pub fn get_snapshot(state: AppState<'_>) -> Option<Snapshot> {
    let mut snap = match state.latest.lock().expect("latest").clone() {
        Some(s) => s,
        None => persist::load_snapshot(&state.data_dir)?,
    };
    // 存檔的那份可能早於後來的帳號或勾選變動：回給前端前先對齊現在的清單
    reconcile(&mut snap, &mirror(&state));
    Some(snap)
}

#[tauri::command]
pub fn refresh_now(state: AppState<'_>) {
    state.force_full.store(true, Ordering::SeqCst);
    state.wake.notify_one();
}

#[tauri::command]
pub async fn login_start(state: AppState<'_>) -> Result<DeviceCode, String> {
    let flow = require_flow(state.flow.as_ref()).map_err(|_| NOT_CONFIGURED)?;
    let code = flow.start().await.map_err(err)?;
    *state.pending_token.lock().expect("pending") = None;
    *state.pending_login.lock().expect("pending") = Some(code.clone());
    Ok(code)
}

/// 把進行中登入的代碼放進剪貼簿，使用者到 GitHub 頁面直接貼上。
/// 只複製 `login_start` 拿到的那組代碼，前端不能藉這個指令寫入任意文字。
#[tauri::command]
pub fn copy_login_code(state: AppState<'_>, app: tauri::AppHandle) -> Result<(), String> {
    let code = state
        .pending_login
        .lock()
        .expect("pending")
        .as_ref()
        .map(|c| c.user_code.clone())
        .ok_or("沒有進行中的登入")?;
    app.clipboard().write_text(code).map_err(err)
}

#[tauri::command]
pub async fn login_poll(device_code: String, state: AppState<'_>) -> Result<LoginStatus, String> {
    let flow = match require_flow(state.flow.as_ref()) {
        Ok(flow) => flow,
        Err(status) => return Ok(status),
    };
    // 上一次授權已經完成、但建立帳號的後半段失敗：裝置代碼已用掉，只重試後半段
    let resumed = state.pending_token.lock().expect("pending").clone();
    let outcome = match resumed {
        Some(token) => PollOutcome::Done(token),
        None => {
            let code = state
                .pending_login
                .lock()
                .expect("pending")
                .clone()
                .filter(|c| c.device_code == device_code)
                .unwrap_or_else(|| DeviceCode {
                    device_code,
                    user_code: String::new(),
                    verification_uri: String::new(),
                    interval: 5,
                    expires_at: 0,
                });
            flow.poll(&code).await.map_err(err)?
        }
    };
    let added = match &outcome {
        PollOutcome::Done(token) => {
            *state.pending_token.lock().expect("pending") = Some(token.clone());
            let added = finish_login(&state, token.clone(), AccountKind::GitHubApp).await?;
            // 帳號、token、收集器都已建立才清掉
            *state.pending_token.lock().expect("pending") = None;
            *state.pending_login.lock().expect("pending") = None;
            Some(added)
        }
        _ => None,
    };
    Ok(login_status_from(&outcome, added.as_ref()))
}

/// 取得帳號身分、預設全選可看的 scope、存 token、加進帳號清單並交給輪詢。
async fn finish_login(
    state: &Shared,
    token: TokenSet,
    kind: AccountKind,
) -> Result<AddedAccount, String> {
    let client = state.new_client(token.access_token.clone());
    let (account_id, login) = api::current_user(&client).await.map_err(err)?;
    let found = discover_scopes(&client, kind).await?;
    let meta = AccountMeta {
        account_id,
        login: login.clone(),
        kind,
        selected_scopes: found.into_iter().map(|(n, _)| n).collect(),
    };
    state.secret_store().put(account_id, &token).map_err(err)?;
    {
        let mut accounts = state.accounts.lock().expect("accounts");
        accounts.retain(|a| a.account_id != account_id);
        accounts.push(meta.clone());
        persist::save_accounts(&state.data_dir, &accounts).map_err(err)?;
    }
    state.push_op(Op::AddAccount {
        meta,
        client: Box::new(client),
    });
    state.force_full.store(true, Ordering::SeqCst);
    Ok(AddedAccount { account_id, login })
}

/// 這個 token 看得到的 org／個人帳號。
async fn discover_scopes(
    client: &pr_runners_core::GitHubClient,
    kind: AccountKind,
) -> Result<Vec<(String, ScopeKind)>, String> {
    Ok(match kind {
        AccountKind::GitHubApp => api::list_installations(client)
            .await
            .map_err(err)?
            .into_iter()
            .map(|i| (i.account_login, i.account_kind))
            .collect(),
        // fine-grained PAT 不能靠 /user/orgs 發現 org：依回傳 repo 的 owner 整理
        AccountKind::Pat => api::list_user_repos(client)
            .await
            .map_err(err)?
            .into_iter()
            .map(|r| (r.owner_login, r.owner_kind))
            .collect(),
    })
}

#[tauri::command]
pub async fn list_scopes(account_id: u64, state: AppState<'_>) -> Result<Vec<ScopeChoice>, String> {
    let meta = mirror(&state)
        .into_iter()
        .find(|a| a.account_id == account_id)
        .ok_or("找不到這個帳號")?;
    let token = state
        .secret_store()
        .get(account_id)
        .map_err(err)?
        .ok_or("金鑰庫裡沒有這個帳號的 token，請重新登入")?;
    let client = state.new_client(token.access_token);
    let found = discover_scopes(&client, meta.kind).await?;
    Ok(scope_choices(&found, &meta.selected_scopes))
}

#[tauri::command]
pub fn set_scopes(
    account_id: u64,
    selected: Vec<String>,
    state: AppState<'_>,
) -> Result<(), String> {
    let selected: BTreeSet<String> = selected.into_iter().collect();
    {
        let mut accounts = state.accounts.lock().expect("accounts");
        if let Some(a) = accounts.iter_mut().find(|a| a.account_id == account_id) {
            a.selected_scopes = selected.clone();
        }
        persist::save_accounts(&state.data_dir, &accounts).map_err(err)?;
    }
    state.push_op(Op::SetScopes {
        account_id,
        selected,
    });
    state.force_full.store(true, Ordering::SeqCst);
    Ok(())
}

/// 帳號從清單、目前顯示的 snapshot 與收集器消失，最後才刪金鑰庫。
/// 順序刻意如此：就算金鑰庫暫時刪不掉，帳號也已經不會再被輪詢；刪不掉的 token 由輪詢套用移除時再刪一次。
/// 與 token 續期互斥（`lifecycle`），續期進行中時會等它結束，避免剛刪掉的 token 又被寫回。
#[tauri::command]
pub async fn remove_account(
    account_id: u64,
    state: AppState<'_>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _lifecycle = state.lifecycle.lock().await;
    // 清單要先成功寫到磁碟才繼續：寫不進去就整個取消（帳號與 token 都原封不動），
    // 免得重開之後帳號還在清單裡、token 卻已經被刪掉
    {
        let mut accounts = state.accounts.lock().expect("accounts");
        let remaining: Vec<AccountMeta> = accounts
            .iter()
            .filter(|a| a.account_id != account_id)
            .cloned()
            .collect();
        persist::save_accounts(&state.data_dir, &remaining).map_err(err)?;
        *accounts = remaining;
    }
    state.push_op(Op::RemoveAccount { account_id });

    // 修剪目前顯示的 snapshot 並推事件：與輪詢的發布共用 `latest` 鎖，最後送出的一定是最新的那份
    {
        let mut latest = state.latest.lock().expect("latest");
        if let Some(snap) = latest.as_mut() {
            reconcile(snap, &mirror(&state));
            let _ = persist::save_snapshot(&state.data_dir, snap);
            let _ = app.emit("snapshot", &*snap);
        }
    }
    // 最後才刪金鑰庫：就算暫時刪不掉，帳號也已經不會再被輪詢，輪詢套用移除時會再刪一次
    state
        .secret_store()
        .delete(account_id)
        .map_err(|e| format!("帳號已移除，但金鑰庫暫時刪不掉 token（會再試一次）：{e}"))
}

#[tauri::command]
pub fn get_settings(state: AppState<'_>) -> Settings {
    load_settings(&state.data_dir)
}

/// 先 clamp 到 500–4,500，存 `settings.json`，下一輪生效；回傳實際採用的值。
#[tauri::command]
pub fn set_hourly_target(n: i64, state: AppState<'_>) -> Result<u32, String> {
    // 前端可能傳負數或超大的數字：先收進 u32 的範圍，再 clamp 到 500–4,500
    let n = clamp_hourly_target(n.clamp(0, i64::from(u32::MAX)) as u32);
    save_settings(
        &state.data_dir,
        &Settings {
            hourly_request_target: n,
            ..load_settings(&state.data_dir)
        },
    )
    .map_err(err)?;
    state.push_op(Op::SetHourlyTarget(n));
    Ok(n)
}

#[tauri::command]
pub fn install_url() -> String {
    install_url_for(option_env!("PR_RUNNERS_GITHUB_APP_SLUG"))
}

/// 備用方案：貼上 fine-grained PAT。
#[tauri::command]
pub async fn add_pat(token: String, state: AppState<'_>) -> Result<AddedAccount, String> {
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("請貼上 token".into());
    }
    finish_login(
        &state,
        TokenSet {
            access_token: token,
            expires_at: None,
            refresh_token: None,
            refresh_expires_at: None,
        },
        AccountKind::Pat,
    )
    .await
}

/// 外部連結一律走這裡，只接受 `https://github.com/`。
/// Rust 端呼叫 opener 不套用前端 capability scope，所以這個檢查不能省。
#[tauri::command]
pub fn open_url(url: String, app: tauri::AppHandle) -> Result<(), String> {
    if !allowed_url(&url) {
        return Err("只能開啟 https://github.com/ 的網址".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(err)
}

/// Android：回到前景 → 解除暫停，下一輪用 Full 且視為第一次看到。Mac 不呼叫。
#[tauri::command]
pub fn app_resumed(state: AppState<'_>) {
    state.halt.store(false, Ordering::SeqCst);
    state.resume_pending.store(true, Ordering::SeqCst);
    state.force_full.store(true, Ordering::SeqCst);
    state.wake.notify_one();
}

/// Android：切到背景 → 不再開始新的一輪，進行中的掃描在下一個請求處中止。Mac 不呼叫。
#[tauri::command]
pub fn app_paused(state: AppState<'_>) {
    state.halt.store(true, Ordering::SeqCst);
}

/// 金鑰庫自我檢測（只有 debug 建置）：put → get → delete 一筆假 token，確認這個平台的金鑰庫真的能用。
/// Android 上用來驗證 Keystore 與 application context 有正確接上。
#[tauri::command]
pub fn secret_selftest(state: AppState<'_>) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("只有 debug 建置提供".into());
    }
    const ID: u64 = 999_000_002; // 不會是真實帳號
    let fake = TokenSet {
        access_token: "selftest-access".into(),
        expires_at: Some(1),
        refresh_token: Some("selftest-refresh".into()),
        refresh_expires_at: Some(2),
    };
    let store = state.secret_store();
    store.put(ID, &fake).map_err(err)?;
    let got = store.get(ID).map_err(err)?;
    store.delete(ID).map_err(err)?;
    if got.as_ref() != Some(&fake) {
        return Err(format!("讀回的內容不一致：{got:?}"));
    }
    if store.get(ID).map_err(err)?.is_some() {
        return Err("刪除後仍讀得到".into());
    }
    Ok(())
}

/// 主視窗的「永遠在最上層」與背景不透明度：存進 settings.json 並立刻套用；回傳實際採用的設定。
/// 不透明度由前端用 CSS 套用（只有背景變透明，文字不受影響）。
#[tauri::command]
pub fn set_window_prefs(
    always_on_top: bool,
    opacity: i64,
    app: tauri::AppHandle,
    state: AppState<'_>,
) -> Result<Settings, String> {
    let s = Settings {
        always_on_top,
        window_opacity: clamp_window_opacity(opacity.clamp(0, 100) as u8),
        ..load_settings(&state.data_dir)
    };
    save_settings(&state.data_dir, &s).map_err(err)?;
    #[cfg(desktop)]
    crate::tray::apply_always_on_top(&app, s.always_on_top);
    #[cfg(not(desktop))]
    let _ = app;
    Ok(s)
}

/// 某台 runner 的執行歷史（新到舊）。讀輪詢迴圈發布的複本，不碰收集器。
#[tauri::command]
pub fn runner_history(runner_key: String, state: AppState<'_>) -> Vec<JobRecord> {
    state.runlog.lock().expect("runlog").records(&runner_key)
}

/// 「載入更早的紀錄」：交給輪詢迴圈補查（收集器只有它能碰），等它回覆結果。
#[tauri::command]
pub async fn backfill_runner_history(
    account_id: u64,
    scope: String,
    runner_name: String,
    runner_key: String,
    state: AppState<'_>,
) -> Result<BackfillReport, String> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    state.push_op(Op::Backfill {
        account_id,
        scope,
        runner_name,
        runner_key,
        reply,
    });
    // 輪詢迴圈可能正在跑一輪（持有收集器）：等它跑完再補查
    match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
        Ok(Ok(report)) => Ok(report),
        Ok(Err(_)) => Err("補查被中斷了，請再試一次".into()),
        Err(_) => Err("補查太久沒有回應，請稍後再試".into()),
    }
}

/// 小視窗點某台 runner：打開主視窗並顯示那台的執行歷史。
#[tauri::command]
pub fn show_runner_history(runner_key: String, app: tauri::AppHandle) {
    #[cfg(desktop)]
    crate::tray::show_runner(&app, &runner_key);
    #[cfg(not(desktop))]
    let _ = (runner_key, app);
}

/// 小視窗底部的「打開主視窗…」「設定…」。
#[tauri::command]
pub fn show_main(settings: bool, app: tauri::AppHandle) {
    #[cfg(desktop)]
    crate::tray::show_main(&app, settings);
    #[cfg(not(desktop))]
    let _ = (settings, app);
}

/// 小視窗底部的「結束」。
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// 小視窗高度依內容，最高 640（400 寬固定）。
#[tauri::command]
pub fn resize_popover(height: f64, app: tauri::AppHandle) {
    #[cfg(desktop)]
    crate::tray::resize_popover(&app, height);
    #[cfg(not(desktop))]
    let _ = (height, app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pr_runners_core::auth::TokenSet;

    #[test]
    fn login_status_maps_every_outcome() {
        use LoginStatus as L;
        assert_eq!(login_status_from(&PollOutcome::Pending, None), L::Pending);
        assert_eq!(
            login_status_from(&PollOutcome::SlowDown { new_interval: 10 }, None),
            L::SlowDown { new_interval: 10 },
            "slow_down 要保留新的間隔"
        );
        assert_eq!(login_status_from(&PollOutcome::Denied, None), L::Denied);
        assert_eq!(login_status_from(&PollOutcome::Expired, None), L::Expired);
        let token = TokenSet {
            access_token: "t".into(),
            expires_at: None,
            refresh_token: None,
            refresh_expires_at: None,
        };
        let alice = AddedAccount {
            account_id: 42,
            login: "alice".into(),
        };
        assert_eq!(
            login_status_from(&PollOutcome::Done(token.clone()), Some(&alice)),
            L::Done {
                login: "alice".into(),
                account_id: 42
            }
        );
        assert_eq!(
            login_status_from(&PollOutcome::Done(token), None),
            L::Pending,
            "沒有帳號資料時不能假裝登入成功"
        );
    }

    #[test]
    fn not_configured_when_there_is_no_client_id() {
        assert_eq!(require_flow(None).err(), Some(LoginStatus::NotConfigured));
        let flow = DeviceFlow::new("Iv1.x");
        assert!(require_flow(Some(&flow)).is_ok());
        assert_eq!(NOT_CONFIGURED, "此版本未設定 GitHub App");
    }

    #[test]
    fn allowed_url_only_accepts_https_github_com() {
        assert!(allowed_url("https://github.com/o/r/actions/runs/1"));
        assert!(allowed_url("https://github.com/login/device"));
        assert!(!allowed_url("http://github.com/o/r"));
        assert!(!allowed_url("https://github.com.evil.com/o/r"));
        assert!(!allowed_url("https://github.com@evil.com/o/r"));
        assert!(!allowed_url("https://github.com:8443/o/r"));
        assert!(!allowed_url("https://user:pw@github.com/o/r"));
        assert!(!allowed_url("https://github.com"));
        assert!(!allowed_url("file:///etc/passwd"));
        assert!(!allowed_url("javascript:alert(1)"));
        assert!(!allowed_url("https://evil.com/https://github.com/"));
        assert!(!allowed_url(""));
    }

    #[test]
    fn scope_choices_marks_selection_sorted_and_deduped() {
        let found = vec![
            ("jdoe".to_string(), ScopeKind::User),
            ("Acme".to_string(), ScopeKind::Org),
            ("Acme".to_string(), ScopeKind::Org),
        ];
        let selected = BTreeSet::from(["Acme".to_string()]);
        assert_eq!(
            scope_choices(&found, &selected),
            vec![
                ScopeChoice {
                    name: "Acme".into(),
                    kind: ScopeKind::Org,
                    selected: true
                },
                ScopeChoice {
                    name: "jdoe".into(),
                    kind: ScopeKind::User,
                    selected: false
                },
            ]
        );
        assert!(scope_choices(&[], &selected).is_empty());
    }

    #[test]
    fn install_url_uses_slug_or_default() {
        assert_eq!(
            install_url_for(None),
            "https://github.com/apps/pr-runners/installations/new"
        );
        assert_eq!(
            install_url_for(Some("")),
            "https://github.com/apps/pr-runners/installations/new"
        );
        assert_eq!(
            install_url_for(Some("pr-runners-dev")),
            "https://github.com/apps/pr-runners-dev/installations/new"
        );
    }
}
