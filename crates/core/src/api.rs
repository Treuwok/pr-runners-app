//! GitHub 端點與 JSON → rules 事實型別的對應。
//!
//! 所有函式只做 GET，錯誤原樣向上回（分類見 `client::ApiError`）。
//! 時間在這裡由 ISO 8601 轉成 Unix 秒 `i64`，之後 crate 之間只傳整數。

use crate::client::{ApiError, GitHubClient};
use pr_runners_rules::{GroupAccess, JobFacts, JobState};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    Deserialize,
    ts_rs::TS,
)]
pub enum ScopeKind {
    Org,
    User,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Installation {
    pub id: u64,
    pub account_login: String,
    pub account_kind: ScopeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoInfo {
    /// owner/name
    pub full_name: String,
    pub owner_login: String,
    pub owner_kind: ScopeKind,
    pub pushed_at: i64,
    pub private: bool,
}

/// API 回應沒有 group 欄位（2026-11-01 實查 keys＝busy,id,labels,name,os,status,version）。
#[derive(Debug, Clone, PartialEq)]
pub struct RawRunner {
    pub id: u64,
    pub name: String,
    pub os: String,
    pub online: bool,
    pub busy: bool,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunInfo {
    pub id: u64,
    pub repo: String,
    /// 舊資料或測試替身可能沒有；沒有就不預估剩餘時間
    pub workflow_id: Option<u64>,
    pub workflow_name: String,
    pub event: String,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub pr_number: Option<u64>,
    pub html_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobInfo {
    pub facts: JobFacts,
    pub run_id: u64,
    pub job_id: u64,
    /// 所屬 workflow（查歷次執行時間用）；沒有就不預估
    pub workflow_id: Option<u64>,
    pub workflow_name: String,
    pub job_name: String,
    pub html_url: String,
    pub run_attempt: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroupInfo {
    pub id: u64,
    pub name: String,
    pub access: GroupAccess,
    pub restricted_to_workflows: bool,
    pub runner_ids: BTreeSet<u64>,
}

// ───────────────────────────── 原始 JSON 形狀 ─────────────────────────────

#[derive(Deserialize)]
struct RawOwner {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct RawRepo {
    full_name: String,
    owner: RawOwner,
    pushed_at: Option<String>,
    #[serde(default)]
    private: bool,
}

#[derive(Deserialize)]
struct RawInstallation {
    id: u64,
    account: RawOwner,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawRunnerJson {
    id: u64,
    name: String,
    #[serde(default)]
    os: String,
    status: String,
    #[serde(default)]
    busy: bool,
    #[serde(default)]
    labels: Vec<RawLabel>,
}

#[derive(Deserialize)]
struct RawGroup {
    id: u64,
    name: String,
    visibility: String,
    #[serde(default)]
    allows_public_repositories: bool,
    #[serde(default)]
    restricted_to_workflows: bool,
}

#[derive(Deserialize)]
struct RawGroupRunner {
    id: u64,
}

#[derive(Deserialize)]
struct RawGroupRepo {
    full_name: String,
}

#[derive(Deserialize)]
struct RawPrRef {
    number: u64,
}

#[derive(Deserialize)]
struct RawRun {
    id: u64,
    #[serde(default)]
    workflow_id: Option<u64>,
    name: Option<String>,
    #[serde(default)]
    event: String,
    head_branch: Option<String>,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    pull_requests: Vec<RawPrRef>,
    #[serde(default)]
    html_url: String,
}

#[derive(Deserialize)]
struct RawJob {
    id: u64,
    run_id: u64,
    name: String,
    status: String,
    #[serde(default)]
    labels: Vec<String>,
    runner_id: Option<u64>,
    runner_name: Option<String>,
    runner_group_name: Option<String>,
    created_at: String,
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    workflow_name: Option<String>,
    #[serde(default = "one")]
    run_attempt: u32,
    #[serde(default)]
    html_url: String,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct RawPull {
    number: u64,
    state: String,
}

// ───────────────────────────── 轉換 ─────────────────────────────

pub fn parse_ts(s: &str) -> Result<i64, ApiError> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map(|t| t.unix_timestamp())
        .map_err(|e| ApiError::Decode(format!("時間 `{s}` 不是 ISO 8601：{e}")))
}

fn scope_kind(owner_type: &str) -> ScopeKind {
    if owner_type.eq_ignore_ascii_case("Organization") {
        ScopeKind::Org
    } else {
        ScopeKind::User
    }
}

fn repo_info(r: RawRepo) -> Result<RepoInfo, ApiError> {
    Ok(RepoInfo {
        full_name: r.full_name,
        owner_login: r.owner.login,
        owner_kind: scope_kind(&r.owner.kind),
        pushed_at: r
            .pushed_at
            .as_deref()
            .map(parse_ts)
            .transpose()?
            .unwrap_or(0),
        private: r.private,
    })
}

fn raw_runner(r: RawRunnerJson) -> RawRunner {
    RawRunner {
        id: r.id,
        name: r.name,
        os: r.os,
        online: r.status == "online",
        busy: r.busy,
        labels: r.labels.into_iter().map(|l| l.name).collect(),
    }
}

// ───────────────────────────── 端點 ─────────────────────────────

pub async fn list_installations(c: &GitHubClient) -> Result<Vec<Installation>, ApiError> {
    let raw: Vec<RawInstallation> = c
        .get_paginated("/user/installations?per_page=100", Some("installations"))
        .await?;
    Ok(raw
        .into_iter()
        .map(|i| Installation {
            id: i.id,
            account_login: i.account.login,
            account_kind: scope_kind(&i.account.kind),
        })
        .collect())
}

pub async fn list_installation_repos(
    c: &GitHubClient,
    installation_id: u64,
) -> Result<Vec<RepoInfo>, ApiError> {
    let raw: Vec<RawRepo> = c
        .get_paginated(
            &format!("/user/installations/{installation_id}/repositories?per_page=100"),
            Some("repositories"),
        )
        .await?;
    raw.into_iter().map(repo_info).collect()
}

/// PAT 帳號（沒有 installation）用。
pub async fn current_user(c: &GitHubClient) -> Result<(u64, String), ApiError> {
    #[derive(Deserialize)]
    struct Me {
        id: u64,
        login: String,
    }
    let me: Me = c.get_json("/user").await?;
    Ok((me.id, me.login))
}

/// 依回傳 repo 的 owner 整理 scope。fine-grained token 呼叫 `/user/orgs` 會得到 200 與空清單，
/// 不能用來發現 org，也不以公開的 `/orgs/{org}/repos` 當作 PAT 已授權的 repo。
pub async fn list_user_repos(c: &GitHubClient) -> Result<Vec<RepoInfo>, ApiError> {
    let raw: Vec<RawRepo> = c
        .get_paginated(
            "/user/repos?affiliation=owner,collaborator,organization_member&per_page=100",
            None,
        )
        .await?;
    raw.into_iter().map(repo_info).collect()
}

pub async fn org_runners(c: &GitHubClient, org: &str) -> Result<Vec<RawRunner>, ApiError> {
    let raw: Vec<RawRunnerJson> = c
        .get_paginated(
            &format!("/orgs/{org}/actions/runners?per_page=100"),
            Some("runners"),
        )
        .await?;
    Ok(raw.into_iter().map(raw_runner).collect())
}

pub async fn repo_runners(c: &GitHubClient, repo: &str) -> Result<Vec<RawRunner>, ApiError> {
    let raw: Vec<RawRunnerJson> = c
        .get_paginated(
            &format!("/repos/{repo}/actions/runners?per_page=100"),
            Some("runners"),
        )
        .await?;
    Ok(raw.into_iter().map(raw_runner).collect())
}

/// runner 屬於哪個 group 只有 `.../runner-groups/{id}/runners` 查得到。
/// 單一 group 的成員或 repo 清單因權限／資料問題取不到 → 該 group 為 `Unknown`；
/// 但 `RateLimited`（以及  401、斷線）向上回傳。
pub async fn org_runner_groups(c: &GitHubClient, org: &str) -> Result<Vec<GroupInfo>, ApiError> {
    let groups: Vec<RawGroup> = c
        .get_paginated(
            &format!("/orgs/{org}/actions/runner-groups?per_page=100"),
            Some("runner_groups"),
        )
        .await?;
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let base = format!("/orgs/{org}/actions/runner-groups/{}", g.id);
        let mut known = true;

        let runner_ids = match c
            .get_paginated::<RawGroupRunner>(
                &format!("{base}/runners?per_page=100"),
                Some("runners"),
            )
            .await
        {
            Ok(rs) => rs.into_iter().map(|r| r.id).collect(),
            Err(e) if e.is_account_wide() => return Err(e),
            Err(_) => {
                known = false;
                BTreeSet::new()
            }
        };

        let access = match g.visibility.as_str() {
            "all" => GroupAccess::All {
                public_ok: g.allows_public_repositories,
            },
            "private" => GroupAccess::Private,
            "selected" => match c
                .get_paginated::<RawGroupRepo>(
                    &format!("{base}/repositories?per_page=100"),
                    Some("repositories"),
                )
                .await
            {
                Ok(repos) => GroupAccess::Selected {
                    repos: repos.into_iter().map(|r| r.full_name).collect(),
                    public_ok: g.allows_public_repositories,
                },
                Err(e) if e.is_account_wide() => return Err(e),
                Err(_) => GroupAccess::Unknown,
            },
            _ => GroupAccess::Unknown,
        };
        out.push(GroupInfo {
            id: g.id,
            name: g.name,
            access: if known { access } else { GroupAccess::Unknown },
            restricted_to_workflows: g.restricted_to_workflows,
            runner_ids,
        });
    }
    Ok(out)
}

fn run_info(repo: &str, r: RawRun) -> RunInfo {
    RunInfo {
        id: r.id,
        repo: repo.to_string(),
        workflow_id: r.workflow_id,
        workflow_name: r.name.unwrap_or_default(),
        event: r.event,
        head_branch: r.head_branch.filter(|b| !b.is_empty()),
        head_sha: r.head_sha,
        pr_number: r.pull_requests.first().map(|p| p.number),
        html_url: r.html_url,
    }
}

/// 進行中＋排隊中＋pending 的 run，按 run id 去重（2026-11-01 實查 pending run 也包含 queued job）。
pub async fn active_runs(c: &GitHubClient, repo: &str) -> Result<Vec<RunInfo>, ApiError> {
    let mut by_id: BTreeMap<u64, RunInfo> = BTreeMap::new();
    for status in ["in_progress", "queued", "pending"] {
        let runs: Vec<RawRun> = c
            .get_paginated(
                &format!("/repos/{repo}/actions/runs?status={status}&per_page=100"),
                Some("workflow_runs"),
            )
            .await?;
        for r in runs {
            by_id.entry(r.id).or_insert_with(|| run_info(repo, r));
        }
    }
    Ok(by_id.into_values().collect())
}

/// 只留 `queued`／`waiting`／`in_progress` 的 job。
/// `runner_id` 為 null／0、`runner_name` 為空，都視為尚未指派。
pub async fn run_jobs(
    c: &GitHubClient,
    run: &RunInfo,
    repo_public: bool,
) -> Result<Vec<JobInfo>, ApiError> {
    let raw: Vec<RawJob> = c
        .get_paginated(
            &format!(
                "/repos/{}/actions/runs/{}/jobs?per_page=100",
                run.repo, run.id
            ),
            Some("jobs"),
        )
        .await?;
    let mut out = Vec::new();
    for j in raw {
        let state = match j.status.as_str() {
            "queued" => JobState::Queued,
            "waiting" => JobState::Waiting,
            "in_progress" => JobState::InProgress,
            _ => continue,
        };
        let created_at = parse_ts(&j.created_at)?;
        let started_at = j.started_at.as_deref().map(parse_ts).transpose()?;
        out.push(JobInfo {
            facts: JobFacts {
                key: format!("{}/{}", j.run_id, j.id),
                repo: run.repo.clone(),
                repo_public,
                state,
                labels: j.labels,
                runner_id: j.runner_id.filter(|id| *id != 0),
                runner_name: j.runner_name.filter(|n| !n.is_empty()),
                runner_group_name: j.runner_group_name.filter(|n| !n.is_empty()),
                created_at,
                started_at,
            },
            run_id: j.run_id,
            job_id: j.id,
            workflow_id: run.workflow_id,
            workflow_name: j.workflow_name.unwrap_or_else(|| run.workflow_name.clone()),
            job_name: j.name,
            html_url: j.html_url,
            run_attempt: j.run_attempt,
        });
    }
    Ok(out)
}

// ───────────────────────────── 歷次執行時間 ─────────────────────────────

#[derive(Deserialize)]
struct RawRunId {
    id: u64,
}

#[derive(Deserialize)]
struct RawRunIds {
    #[serde(default)]
    workflow_runs: Vec<RawRunId>,
}

/// 某個 workflow 最近 `n` 次成功的執行（新到舊），只取一頁：要的是最近幾次，不是全部歷史。
pub async fn successful_run_ids(
    c: &GitHubClient,
    repo: &str,
    workflow_id: u64,
    n: u32,
) -> Result<Vec<u64>, ApiError> {
    let page: RawRunIds = c
        .get_json(&format!(
            "/repos/{repo}/actions/workflows/{workflow_id}/runs?status=success&per_page={n}"
        ))
        .await?;
    Ok(page.workflow_runs.into_iter().map(|r| r.id).collect())
}

/// 一次已完成的執行裡，每個成功的 job 跑了多久。被略過、失敗、取消的 job 不算
/// （略過的 job 很常見：路徑過濾或 ci:hold 會讓重的 job 整批不跑）。
pub async fn completed_job_durations(
    c: &GitHubClient,
    repo: &str,
    run_id: u64,
) -> Result<Vec<crate::history::Sample>, ApiError> {
    let raw: Vec<RawJob> = c
        .get_paginated(
            &format!("/repos/{repo}/actions/runs/{run_id}/jobs?per_page=100"),
            Some("jobs"),
        )
        .await?;
    let mut out = Vec::new();
    for j in raw {
        if j.conclusion.as_deref() != Some("success") {
            continue;
        }
        let (Some(start), Some(end)) = (j.started_at.as_deref(), j.completed_at.as_deref()) else {
            continue;
        };
        let secs = parse_ts(end)? - parse_ts(start)?;
        if secs > 0 {
            out.push(crate::history::Sample {
                job_name: j.name,
                secs: secs as u64,
                runner: j.runner_name.filter(|n| !n.is_empty()),
            });
        }
    }
    Ok(out)
}

// ───────────────────────────── runner 執行歷史 ─────────────────────────────

/// 一個 job 的完整狀態（含已完成的），給 runner 執行歷史用。
#[derive(Debug, Clone, PartialEq)]
pub struct JobSnapshot {
    pub job_id: u64,
    pub run_id: u64,
    pub name: String,
    pub workflow_name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub runner_id: Option<u64>,
    pub runner_name: Option<String>,
    pub html_url: String,
}

fn job_snapshot(j: RawJob, workflow_fallback: &str) -> Result<JobSnapshot, ApiError> {
    Ok(JobSnapshot {
        job_id: j.id,
        run_id: j.run_id,
        name: j.name,
        workflow_name: j
            .workflow_name
            .unwrap_or_else(|| workflow_fallback.to_string()),
        status: j.status,
        conclusion: j.conclusion,
        started_at: j.started_at.as_deref().map(parse_ts).transpose()?,
        completed_at: j.completed_at.as_deref().map(parse_ts).transpose()?,
        runner_id: j.runner_id.filter(|id| *id != 0),
        runner_name: j.runner_name.filter(|n| !n.is_empty()),
        html_url: j.html_url,
    })
}

/// 單一 job 現在的狀態（job 從畫面上消失後，查它最後是成功還是失敗）。
pub async fn job_status(
    c: &GitHubClient,
    repo: &str,
    job_id: u64,
) -> Result<JobSnapshot, ApiError> {
    let j: RawJob = c
        .get_json(&format!("/repos/{repo}/actions/jobs/{job_id}"))
        .await?;
    job_snapshot(j, "")
}

#[derive(Deserialize)]
struct RawRunPage {
    #[serde(default)]
    workflow_runs: Vec<RawRun>,
}

/// repo 最近 `n` 次已完成的執行（新到舊），只取一頁。
pub async fn recent_completed_runs(
    c: &GitHubClient,
    repo: &str,
    n: u32,
) -> Result<Vec<RunInfo>, ApiError> {
    let page: RawRunPage = c
        .get_json(&format!(
            "/repos/{repo}/actions/runs?status=completed&per_page={n}"
        ))
        .await?;
    Ok(page
        .workflow_runs
        .into_iter()
        .map(|r| run_info(repo, r))
        .collect())
}

/// 一次執行的所有 job（含已完成的）。
pub async fn run_jobs_all(c: &GitHubClient, run: &RunInfo) -> Result<Vec<JobSnapshot>, ApiError> {
    let raw: Vec<RawJob> = c
        .get_paginated(
            &format!(
                "/repos/{}/actions/runs/{}/jobs?per_page=100",
                run.repo, run.id
            ),
            Some("jobs"),
        )
        .await?;
    raw.into_iter()
        .map(|j| job_snapshot(j, &run.workflow_name))
        .collect()
}

// ───────────────────────────── PR 標籤 ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct PullRef {
    number: u64,
    open: bool,
}

/// commit → PR 查詢結果，以 `(repo, head_sha)` 快取，同一個 sha 不重查。
/// 只快取查詢結果；最終標籤仍依每個 run 的 event／branch／pr_number 組成，
/// 避免同 SHA 的 push 與 pull_request run 共用錯誤標籤。
#[derive(Default)]
pub struct PrTagCache {
    pulls: Mutex<HashMap<(String, String), Option<PullRef>>>,
}

/// 長時間執行的 App 不能讓這張表無限長大；滿了就清空（重查只花一個請求）。
const MAX_PR_CACHE_ENTRIES: usize = 5_000;

impl PrTagCache {
    async fn first_pull(
        &self,
        c: &GitHubClient,
        run: &RunInfo,
    ) -> Result<Option<PullRef>, ApiError> {
        let key = (run.repo.clone(), run.head_sha.clone());
        if let Some(hit) = self.pulls.lock().expect("pr cache").get(&key) {
            return Ok(hit.clone());
        }
        let pulls: Vec<RawPull> = c
            .get_json(&format!(
                "/repos/{}/commits/{}/pulls",
                run.repo, run.head_sha
            ))
            .await?;
        let first = pulls.into_iter().next().map(|p| PullRef {
            number: p.number,
            open: p.state == "open",
        });
        let mut cache = self.pulls.lock().expect("pr cache");
        if cache.len() >= MAX_PR_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, first.clone());
        Ok(first)
    }
}

/// 規則照搬 `bin/pr-runners` 的 `_scan_repo`：
/// - run 本身帶 PR 號 → `PR#N`
/// - push 事件 → `push:{branch}`，反查到來源 PR 再補 `←#N`
/// - 其他 → 反查 commit：open → `PR#N`、非 open → `PR#N(closed)`，查不到 → `{event}:{branch}`
///
/// 只在真的要顯示時才呼叫（沒有 self-hosted job 的 run 不必花這個請求）。
/// 不查 API 就能組出的標籤：`PR#N`，或 `{event}:{branch}`（沒有 branch 就只有 event）。
pub fn local_tag(run: &RunInfo) -> String {
    if let Some(n) = run.pr_number {
        return format!("PR#{n}");
    }
    with_branch(&run.event, run.head_branch.as_deref())
}

fn with_branch(prefix: &str, branch: Option<&str>) -> String {
    match branch {
        Some(b) if !b.is_empty() => format!("{prefix}:{b}"),
        _ => prefix.to_string(),
    }
}

pub async fn pr_tag(
    c: &GitHubClient,
    cache: &PrTagCache,
    run: &RunInfo,
) -> Result<String, ApiError> {
    if run.pr_number.is_some() {
        return Ok(local_tag(run));
    }
    let found = cache.first_pull(c, run).await?;
    Ok(if run.event == "push" {
        let push = with_branch("push", run.head_branch.as_deref());
        match found {
            Some(p) => format!("{push}←#{}", p.number),
            None => push,
        }
    } else {
        match found {
            Some(p) if p.open => format!("PR#{}", p.number),
            Some(p) => format!("PR#{}(closed)", p.number),
            None => local_tag(run),
        }
    })
}
