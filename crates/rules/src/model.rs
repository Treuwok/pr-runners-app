//! rules 與 core 之間傳遞的事實型別。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 一段經過的時間。GitHub 的時間戳可能偏移（2026-11-01 整批多 1 小時），
/// 不可信時不硬算，交給 UI 顯示 `⚠?`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Elapsed {
    Known { secs: i64 },
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum JobState {
    Queued,
    /// 等 environment 核准之類；不是缺 runner，不進「等待中」，也不觸發排隊太久。
    Waiting,
    InProgress,
}

/// runner 所屬 group 對 repo 的授權。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum GroupAccess {
    /// visibility = all。`public_ok` 是 group 的 `allows_public_repositories`。
    All { public_ok: bool },
    /// visibility = private：只服務 private repo。
    Private,
    /// visibility = selected。`repos` 是 repo 全名（owner/name）。
    /// repo 層級的 runner 也用這個形狀：`Selected { repos: {自己的 repo}, public_ok: true }`。
    Selected {
        repos: BTreeSet<String>,
        public_ok: bool,
    },
    /// 權限資料取不到，或 runner 不屬於任何已知 group。
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunnerFacts {
    /// `"{account_id}/{註冊 scope}/{runner_id}"`，同名 runner 不會互相混淆。
    pub key: String,
    pub id: u64,
    pub name: String,
    pub online: bool,
    pub busy: bool,
    pub labels: Vec<String>,
    pub group_access: GroupAccess,
    /// group 開了 `restricted_to_workflows`：已知的 repo／public 限制照樣適用，
    /// 通過後只能算「可能能接」。
    pub restricted_to_workflows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JobFacts {
    /// `"{run_id}/{job_id}"`
    pub key: String,
    /// owner/name
    pub repo: String,
    pub repo_public: bool,
    pub state: JobState,
    pub labels: Vec<String>,
    pub runner_id: Option<u64>,
    pub runner_name: Option<String>,
    pub runner_group_name: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
}
