//! Snapshot 與 view 型別：前端只吃這一份。透過 ts-rs 匯出到 `app/src/lib/generated/`。
//! 時間與 ID 都是整數（TS 端為 `number`，見 `.cargo/config.toml`）。

use crate::api::ScopeKind;
use pr_runners_rules::{Candidate, CandidateKind, Elapsed, JobState, OfflineSince};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    pub schema_version: u32,
    pub generated_at: i64,
    pub accounts: Vec<AccountView>,
    pub summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Summary {
    pub busy: u32,
    pub idle: u32,
    pub offline: u32,
    pub waiting: u32,
    pub attention: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Severity {
    Ok,
    /// 有排隊太久的 job
    Warn,
    /// 有依 spec §10 判定需要注意的離線 runner（紅優先於橘）
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountView {
    pub account_id: u64,
    pub login: String,
    pub status: AccountStatus,
    pub scopes: Vec<ScopeView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum AccountStatus {
    Ok,
    /// token 失效、或 App 被解除安裝：該帳號顯示「重新登入」
    NeedsReauth,
    /// 被限流，或剩餘配額低於 20% 而放慢更新
    Throttled {
        remaining: u32,
        reset_at: i64,
    },
    /// 沒有網路：scopes 沿用上一份，標示「最後更新」
    Offline {
        last_ok_at: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum RosterLevel {
    /// 拿得到完整名冊（含閒置／離線）
    Full,
    /// 權限不足：只看得到正在跑的 runner（從 job 反推）
    BusyOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScopeView {
    pub name: String,
    pub kind: ScopeKind,
    pub roster: RosterLevel,
    pub runners: Vec<RunnerView>,
    pub waiting: Vec<WaitingView>,
    /// 所有進行中、排隊中、等待核准的 job（含 GitHub-hosted），給 Jobs 頁
    pub jobs: Vec<JobView>,
    pub last_updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum RunnerState {
    Busy,
    Idle,
    Offline { since: OfflineSince },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunnerView {
    pub key: String,
    pub name: String,
    pub os: String,
    pub state: RunnerState,
    pub labels: Vec<String>,
    pub group: Option<String>,
    pub job: Option<JobView>,
    pub elapsed: Option<Elapsed>,
    pub attention: bool,
    /// 是否被某筆等待中 job 標成能接（取最確定的一種）
    pub can_take: Option<CandidateKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WaitingView {
    pub job: JobView,
    pub age: Elapsed,
    pub attention: bool,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobView {
    pub key: String,
    pub repo: String,
    pub workflow: String,
    pub job: String,
    pub state: JobState,
    /// 等待中區塊顯示「需要的 labels」（spec §5）
    pub labels: Vec<String>,
    pub pr_tag: String,
    pub url: String,
    pub runner_name: Option<String>,
    /// 跑在 GitHub-hosted runner 上（不是自家，也不是 Blacksmith）
    pub hosted: bool,
    /// 跑在 Blacksmith 的一次性 VM 上（ci-burst 溢出）：與自家 runner、GitHub-hosted 分開顯示。
    /// 預設 false：升級前存下的快照沒有這個欄位，照樣讀得進來（那時 Blacksmith job 還不會被分出來）。
    #[serde(default)]
    pub blacksmith: bool,
    /// 同名 job 最近幾次成功平常跑多久（秒）；紀錄不足就是 null，畫面不預估
    pub typical_secs: Option<u64>,
}
