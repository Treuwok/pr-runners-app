//! App 的共用狀態。
//!
//! 收集器**只有背景輪詢會碰**：一輪 tick 要持有它跨好幾秒的網路請求，如果 commands 也直接鎖它，
//! 使用者改勾選或移除帳號時畫面會跟著卡住。所以 commands 改成「更新帳號清單的鏡像（立刻生效、立刻存檔）
//! ＋ 丟一個 `Op` 進佇列」，由輪詢迴圈在每一輪開始前套用到收集器。這同時讓「移除帳號」與
//! 「token 續期」在同一個迴圈裡序列化，不會出現續期成功後把剛刪掉的 token 又寫回金鑰庫。

use pr_runners_core::auth::{DeviceCode, DeviceFlow, TokenSet};
use pr_runners_core::persist::AccountMeta;
use pr_runners_core::persist::save_runlog;
use pr_runners_core::runlog::{BackfillReport, RunLog};
use pr_runners_core::secrets::{KeyringStore, RetryingStore, SecretStore};
use pr_runners_core::view::Snapshot;
use pr_runners_core::{Collector, GitHubClient};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use url::Url;

/// 輪詢迴圈在下一輪開始前套用到收集器的變更。
pub enum Op {
    AddAccount {
        meta: AccountMeta,
        client: Box<GitHubClient>,
    },
    SetScopes {
        account_id: u64,
        selected: BTreeSet<String>,
    },
    RemoveAccount {
        account_id: u64,
    },
    SetHourlyTarget(u32),
    /// 「載入更早的紀錄」：補查完把結果傳回等待中的指令
    Backfill {
        account_id: u64,
        scope: String,
        runner_name: String,
        runner_key: String,
        reply: tokio::sync::oneshot::Sender<BackfillReport>,
    },
}

pub struct Shared {
    pub data_dir: PathBuf,
    pub api_base: Url,
    /// 只有背景輪詢會鎖它。
    pub collector: tokio::sync::Mutex<Collector>,
    /// 帳號生命週期鎖：token 續期（含寫金鑰庫）與移除帳號（含刪金鑰庫）互斥。
    /// 沒有它，續期在途時移除帳號會讓剛刪掉的 token 又被寫回金鑰庫。
    pub lifecycle: tokio::sync::Mutex<()>,
    pub ops: Mutex<Vec<Op>>,
    /// 帳號清單的唯一寫入點（commands 立刻更新並存檔，輪詢照它套用到收集器）。
    pub accounts: Mutex<Vec<AccountMeta>>,
    /// 最新一份 snapshot，`get_snapshot` 讀它。
    pub latest: Mutex<Option<Snapshot>>,
    /// runner 執行歷史的最新複本（輪詢迴圈發布、`runner_history` 讀它；收集器本身只有迴圈碰）
    pub runlog: Mutex<RunLog>,
    /// 複本有變動、但還沒成功存到磁碟（存檔失敗時留著，下一次發布再試）
    pub runlog_dirty: AtomicBool,
    /// 金鑰庫寫入失敗時，新的 token 留在記憶體並重試。
    pub secrets: RetryingStore<KeyringStore>,
    /// None＝建置時沒有設定 `PR_RUNNERS_GITHUB_CLIENT_ID`
    pub flow: Option<DeviceFlow>,
    pub pending_login: Mutex<Option<DeviceCode>>,
    /// 授權已完成、但建立帳號的後半段失敗：裝置代碼已經被用掉，不能重新輪詢，所以留著 token 重試後半段。
    pub pending_token: Mutex<Option<TokenSet>>,
    /// 喚醒輪詢迴圈：手動刷新、回前景、帳號或勾選變動
    pub wake: Notify,
    /// Android 在背景：迴圈不開始新的一輪；所有 HTTP client 共用這個旗標，進行中的掃描也會在下一個請求處中止。
    pub halt: Arc<AtomicBool>,
    pub force_full: AtomicBool,
    /// 回到前景：下一輪每個 scope 都視為「第一次看到」
    pub resume_pending: AtomicBool,
}

/// 有變動就更新複本並標記待存；有待存就試著存，存成功才清掉標記。
/// 不能只靠「複本與收集器是否相同」判斷要不要存：那次存檔失敗後兩者已經相同，之後就再也不會存。
pub fn publish_runlog_copy(
    dir: &std::path::Path,
    copy: &mut RunLog,
    latest: &RunLog,
    dirty: &AtomicBool,
) {
    if *copy != *latest {
        *copy = latest.clone();
        dirty.store(true, Ordering::SeqCst);
    }
    if dirty.load(Ordering::SeqCst) && save_runlog(dir, copy).is_ok() {
        dirty.store(false, Ordering::SeqCst);
    }
}

/// 讓一份剛掃完的 snapshot 符合「現在的帳號清單」：已移除的帳號與已取消勾選的 scope 都拿掉。
/// 掃描進行中使用者可能剛好移除帳號或改勾選；沒有這一步，那一輪會把剛消失的東西又推給畫面與存檔。
pub fn reconcile(snap: &mut Snapshot, accounts: &[AccountMeta]) {
    snap.accounts
        .retain(|a| accounts.iter().any(|m| m.account_id == a.account_id));
    for a in &mut snap.accounts {
        if let Some(m) = accounts.iter().find(|m| m.account_id == a.account_id) {
            a.scopes.retain(|s| m.selected_scopes.contains(&s.name));
        }
    }
    snap.summary = pr_runners_core::assemble::summarize(&snap.accounts);
}

impl Shared {
    pub fn new_client(&self, token: String) -> GitHubClient {
        GitHubClient::new(self.api_base.clone(), token).with_halt(self.halt.clone())
    }

    pub fn secret_store(&self) -> &dyn SecretStore {
        &self.secrets
    }

    pub fn push_op(&self, op: Op) {
        self.ops.lock().expect("ops").push(op);
        self.wake.notify_one();
    }
}

#[cfg(test)]
mod runlog_tests {
    use super::*;
    use pr_runners_core::persist::load_runlog;
    use pr_runners_core::runlog::{JobOutcome, JobRecord, RecordSource};

    fn sample() -> RunLog {
        let mut log = RunLog::default();
        log.insert(
            "1/A/9",
            1,
            JobRecord {
                job_id: 5,
                run_id: 4,
                repo: "A/r".into(),
                workflow: "ci".into(),
                job: "build".into(),
                pr_tag: "PR#1".into(),
                url: String::new(),
                started_at: Some(10),
                completed_at: Some(20),
                outcome: JobOutcome::Success,
                source: RecordSource::Observed,
                checked_at: 20,
            },
        );
        log
    }

    #[test]
    fn failed_save_is_retried_on_the_next_publish_even_without_new_changes() {
        let base = std::env::temp_dir().join(format!("pr-runners-runlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // 存檔位置是一個「檔案」而不是資料夾 → 存檔失敗
        let not_a_dir = base.join("file");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let (mut copy, latest, dirty) = (RunLog::default(), sample(), AtomicBool::new(false));
        publish_runlog_copy(&not_a_dir, &mut copy, &latest, &dirty);
        assert_eq!(copy, latest, "複本照樣更新（畫面讀得到）");
        assert!(dirty.load(Ordering::SeqCst), "存失敗：標記留著");
        // 之後沒有新變動，但磁碟恢復了：要補存
        let ok_dir = base.join("data");
        publish_runlog_copy(&ok_dir, &mut copy, &latest, &dirty);
        assert!(!dirty.load(Ordering::SeqCst));
        assert_eq!(load_runlog(&ok_dir), latest);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pr_runners_core::api::ScopeKind;
    use pr_runners_core::persist::AccountKind;
    use pr_runners_core::view::{
        AccountStatus, AccountView, RosterLevel, ScopeView, Severity, Summary,
    };

    fn scope(name: &str) -> ScopeView {
        ScopeView {
            name: name.into(),
            kind: ScopeKind::Org,
            roster: RosterLevel::Full,
            runners: vec![],
            waiting: vec![],
            jobs: vec![],
            last_updated_at: 0,
        }
    }
    fn account(id: u64, scopes: &[&str]) -> AccountView {
        AccountView {
            account_id: id,
            login: format!("u{id}"),
            status: AccountStatus::Ok,
            scopes: scopes.iter().map(|n| scope(n)).collect(),
        }
    }
    fn meta(id: u64, selected: &[&str]) -> AccountMeta {
        AccountMeta {
            account_id: id,
            login: format!("u{id}"),
            kind: AccountKind::Pat,
            selected_scopes: selected.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn snap(accounts: Vec<AccountView>) -> Snapshot {
        Snapshot {
            schema_version: 1,
            generated_at: 1,
            accounts,
            summary: Summary {
                own_busy: 9,
                own_online: 9,
                own_offline: 9,
                pool_busy: 9,
                waiting: 9,
                attention: Severity::Warn,
                stale: true,
            },
        }
    }

    #[test]
    fn reconcile_drops_removed_accounts_and_unselected_scopes() {
        let mut s = snap(vec![account(1, &["A", "B"]), account(2, &["C"])]);
        reconcile(&mut s, &[meta(1, &["A"])]);
        assert_eq!(s.accounts.len(), 1, "帳號 2 已被移除");
        let names: Vec<_> = s.accounts[0]
            .scopes
            .iter()
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(names, ["A"], "B 已取消勾選");
        assert_eq!(s.summary.own_busy, 0, "計數要依剩下的內容重算");
        assert_eq!(s.summary.attention, Severity::Ok);
    }

    #[test]
    fn reconcile_keeps_everything_that_is_still_wanted() {
        let mut s = snap(vec![account(1, &["A"])]);
        let before = s.accounts.clone();
        reconcile(&mut s, &[meta(1, &["A", "Z"])]);
        assert_eq!(s.accounts, before);
    }
}
