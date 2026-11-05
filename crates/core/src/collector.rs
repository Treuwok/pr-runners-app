//! 收集器：熱冷分層、完整掃描、每小時請求目標、輪詢間隔。
//!
//! 每個帳號平行、同帳號內以 `Semaphore(8)` 限制同時請求的 repo 數。
//! 觀察狀態與上一輪 attention 按 scope 分開保存——共用一份會讓後處理的 scope
//! 把前一個 scope 的離線起點與 job 首見時間清掉（`evaluate` 會移除不在輸入裡的 key）。

use crate::api::{self, GroupInfo, JobInfo, PrTagCache, RawRunner, RepoInfo, ScopeKind};
use crate::assemble::{JobEntry, RunnerRec, ScopeInput, assemble, parse_runner_key, summarize};
use crate::client::{ApiError, GitHubClient};
use crate::history::{JobHistory, WorkflowKey};
use crate::persist::{AccountKind, AccountMeta, clamp_hourly_target};
use crate::runlog::{BackfillReport, RunLog, backfill_records};
use crate::view::*;
use futures_util::future::join_all;
use pr_runners_rules::{Attention, Fleet, GroupAccess, ObservationState, needs_self_hosted};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio::sync::Semaphore;

const HOT_WINDOW_SECS: i64 = 24 * 3_600;
const COLD_INTERVAL_SECS: i64 = 600;
const GROUP_INTERVAL_SECS: i64 = 1_800;
/// 完整掃描距上次不到這麼久，降為一般輪詢。
const FULL_SCAN_MIN_GAP_SECS: i64 = 120;
const BUSY_DELAY_SECS: u64 = 30;
const IDLE_DELAY_SECS: u64 = 120;
const SLOWED_MAX_DELAY_SECS: u64 = 600;
const MAX_PARALLEL_REPOS: usize = 8;
/// runner 執行歷史：每個帳號每輪最多查幾個「剛結束的 job」的結果。
const MAX_RESULTS_PER_TICK: usize = 10;
/// 補查：只看最近幾天有推送的 repo、最多幾個 repo、每個 repo 最近幾次執行。
const BACKFILL_PUSHED_WITHIN_SECS: i64 = 7 * 24 * 3_600;
const BACKFILL_REPOS: usize = 6;
const BACKFILL_RUNS_PER_REPO: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMode {
    /// 啟動、回前景、手動刷新：所有勾選範圍內的 repo 全部查一次。
    Full,
    Incremental,
}

/// 這一輪是不是「掃全部」：完整掃描，或冷 repo（與 repo 清單）每 600 秒一次的窗口到了。
fn is_sweep(mode: TickMode, now: i64, last_cold_scan: i64) -> bool {
    mode == TickMode::Full || now - last_cold_scan >= COLD_INTERVAL_SECS
}

/// 這一輪要掃哪些 repo（熱冷分層，純函式）。
/// 熱＝`pushed_at` 在 24 小時內，或上一輪有進行中／排隊中的 run；冷 repo 每 600 秒一次。
pub fn select_repos(
    repos: &[RepoInfo],
    last_active: &BTreeSet<String>,
    last_cold_scan: i64,
    now: i64,
    mode: TickMode,
) -> Vec<String> {
    let all = is_sweep(mode, now, last_cold_scan);
    repos
        .iter()
        .filter(|r| {
            all || r.pushed_at >= now - HOT_WINDOW_SECS || last_active.contains(&r.full_name)
        })
        .map(|r| r.full_name.clone())
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub struct DelayInputs {
    pub has_work: bool,
    /// 任一帳號的 `remaining` 低於 `limit` 的 20%
    pub low_quota: bool,
    /// 以一般輪詢間隔（30／120 秒）的成本推估，一小時會花多少計費請求
    pub projected_per_hour: u32,
    pub hourly_target: u32,
}

/// 有進行中或排隊中的 job → 30 秒，否則 120 秒。
pub fn base_delay_secs(has_work: bool) -> u64 {
    if has_work {
        BUSY_DELAY_SECS
    } else {
        IDLE_DELAY_SECS
    }
}

/// 推估會超過每小時目標 → 間隔每加倍一次，用量就減半，加倍到不超過為止（上限 600 秒）；
/// 配額低於 20% 再多乘 2。
pub fn compute_delay(i: &DelayInputs) -> Duration {
    let base = base_delay_secs(i.has_work);
    let mut secs = base;
    while secs < SLOWED_MAX_DELAY_SECS
        && u64::from(i.projected_per_hour) * base > u64::from(i.hourly_target) * secs
    {
        secs = (secs * 2).min(SLOWED_MAX_DELAY_SECS);
    }
    if i.low_quota {
        secs *= 2;
    }
    Duration::from_secs(secs.min(SLOWED_MAX_DELAY_SECS))
}

struct RunEntry {
    tag: String,
    jobs: Vec<JobInfo>,
}

struct RepoScan {
    /// repo 層級 runner 名冊；None＝這次沒取（沿用快取）
    runners: Option<Vec<RawRunner>>,
    /// 這個 repo 的 runner 名冊沒有權限（403／404）
    runners_forbidden: bool,
    runs: Vec<RunEntry>,
}

#[derive(Default)]
struct ScopeRuntime {
    state: ObservationState,
    attention: Vec<Attention>,
    /// 啟動（或回前景）後是否已成功評估過；沒有就是 first_observation
    observed: bool,
    roster: Option<RosterLevel>,
    org_runners: Vec<RawRunner>,
    repo_runners: BTreeMap<String, Vec<RawRunner>>,
    /// 沒有權限讀 runner 名冊的 repo：只在掃全部的窗口重試，免得每輪都白打一個 403
    no_runner_access: BTreeSet<String>,
    repo_runs: BTreeMap<String, Vec<RunEntry>>,
    groups: Option<(i64, Vec<GroupInfo>)>,
}

struct TickEnv {
    now: i64,
    mode: TickMode,
}

/// 掃描時要用的帳號資源。PR 標籤快取跟著帳號走：不同帳號的 Pull requests 權限可能不同，
/// 不能共用同一份 commit → PR 的查詢結果。
struct Ctx<'a> {
    client: &'a GitHubClient,
    sem: &'a Semaphore,
    pr_cache: &'a PrTagCache,
    account_id: u64,
}

/// 讓某個帳號暫時不掃（例如 token 續期失敗），畫面照樣顯示對應狀態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// refresh token 無效或過期：要重新登入
    NeedsReauth,
    /// 續期時斷線：下一輪再試
    Offline,
}

struct Account {
    hold: Option<Hold>,
    meta: AccountMeta,
    client: GitHubClient,
    sem: Semaphore,
    pr_cache: PrTagCache,
    repos: Vec<RepoInfo>,
    scope_kinds: BTreeMap<String, ScopeKind>,
    /// 0＝還沒掃過
    last_cold_scan: i64,
    scopes: BTreeMap<String, ScopeRuntime>,
    last_view: Vec<ScopeView>,
    last_ok_at: i64,
    /// 歷次執行時間（預估剩餘時間用）
    history: JobHistory,
}

impl Account {
    fn new(
        meta: AccountMeta,
        client: GitHubClient,
        scopes: BTreeMap<String, ScopeRuntime>,
    ) -> Self {
        Self {
            hold: None,
            meta,
            client,
            sem: Semaphore::new(MAX_PARALLEL_REPOS),
            pr_cache: PrTagCache::default(),
            repos: Vec::new(),
            scope_kinds: BTreeMap::new(),
            last_cold_scan: 0,
            scopes,
            last_view: Vec::new(),
            last_ok_at: 0,
            history: JobHistory::default(),
        }
    }

    async fn run(&mut self, env: &TickEnv) -> AccountView {
        if let Some(hold) = self.hold {
            let status = match hold {
                Hold::NeedsReauth => AccountStatus::NeedsReauth,
                Hold::Offline => AccountStatus::Offline {
                    last_ok_at: self.last_ok_at,
                },
            };
            return AccountView {
                account_id: self.meta.account_id,
                login: self.meta.login.clone(),
                status,
                scopes: self.last_view.clone(),
            };
        }
        let result = self.scan(env).await;
        let rate = self.client.rate();
        let (status, scopes) = match result {
            Ok(scopes) => {
                self.last_ok_at = env.now;
                self.last_view = scopes.clone();
                let status = if rate.is_low() {
                    AccountStatus::Throttled {
                        remaining: rate.remaining,
                        reset_at: rate.reset_at,
                    }
                } else {
                    AccountStatus::Ok
                };
                (status, scopes)
            }
            Err(ApiError::Unauthorized) => (AccountStatus::NeedsReauth, self.last_view.clone()),
            Err(ApiError::RateLimited { reset_at }) => (
                AccountStatus::Throttled {
                    remaining: rate.remaining,
                    reset_at,
                },
                self.last_view.clone(),
            ),
            Err(_) => (
                AccountStatus::Offline {
                    last_ok_at: self.last_ok_at,
                },
                self.last_view.clone(),
            ),
        };
        AccountView {
            account_id: self.meta.account_id,
            login: self.meta.login.clone(),
            status,
            scopes,
        }
    }

    async fn scan(&mut self, env: &TickEnv) -> Result<Vec<ScopeView>, ApiError> {
        // 歷史補抓的額度是整個帳號一輪共用，不是每個範圍各一份
        self.history.start_tick();
        let sweep = is_sweep(env.mode, env.now, self.last_cold_scan);
        if sweep {
            self.discover().await?;
        }
        let targets: Vec<(String, ScopeKind)> = self
            .scope_kinds
            .iter()
            .filter(|(name, _)| self.meta.selected_scopes.contains(*name))
            .map(|(n, k)| (n.clone(), *k))
            .collect();
        // 取消勾選的 scope 不留狀態（也不會被存到磁碟）
        self.scopes
            .retain(|name, _| self.meta.selected_scopes.contains(name));

        let mut views = Vec::with_capacity(targets.len());
        for (name, kind) in targets {
            let repos: Vec<RepoInfo> = self
                .repos
                .iter()
                .filter(|r| r.owner_login == name)
                .cloned()
                .collect();
            let mut rt = self.scopes.remove(&name).unwrap_or_default();
            let ctx = Ctx {
                client: &self.client,
                sem: &self.sem,
                pr_cache: &self.pr_cache,
                account_id: self.meta.account_id,
            };
            let view = scan_scope(
                &ctx,
                &name,
                kind,
                &repos,
                &mut rt,
                env,
                self.last_cold_scan,
                &mut self.history,
            )
            .await;
            self.scopes.insert(name, rt);
            views.push(view?);
        }
        if sweep {
            self.last_cold_scan = env.now;
        }
        Ok(views)
    }

    /// repo 清單（含 `pushed_at`）與 scope 種類。
    async fn discover(&mut self) -> Result<(), ApiError> {
        let mut repos = Vec::new();
        let mut kinds = BTreeMap::new();
        match self.meta.kind {
            AccountKind::GitHubApp => {
                for ins in api::list_installations(&self.client).await? {
                    kinds.insert(ins.account_login.clone(), ins.account_kind);
                    if !self.meta.selected_scopes.contains(&ins.account_login) {
                        continue;
                    }
                    match api::list_installation_repos(&self.client, ins.id).await {
                        Ok(r) => repos.extend(r),
                        // installation 404＝App 被解除安裝：該帳號要重新登入
                        Err(ApiError::NotFound) => return Err(ApiError::Unauthorized),
                        Err(e) => return Err(e),
                    }
                }
            }
            AccountKind::Pat => {
                for r in api::list_user_repos(&self.client).await? {
                    kinds.entry(r.owner_login.clone()).or_insert(r.owner_kind);
                    repos.push(r);
                }
            }
        }
        self.repos = repos;
        self.scope_kinds = kinds;
        Ok(())
    }
}

/// 這個 job 要不要為它查「平常跑多久」：正在自家（self-hosted）runner 上跑的才要；跑在臨時機器上的不要。
fn wants_history(j: &JobInfo, fleet: &Fleet) -> bool {
    j.facts.state == pr_runners_rules::JobState::InProgress
        && needs_self_hosted(&j.facts)
        && j.facts
            .runner_name
            .as_deref()
            .is_none_or(|n| fleet.stem_of(n).is_none())
}

async fn scan_repo(
    ctx: &Ctx<'_>,
    repo: &RepoInfo,
    want_runners: bool,
) -> Result<RepoScan, ApiError> {
    let (c, cache) = (ctx.client, ctx.pr_cache);
    let _permit = ctx.sem.acquire().await.expect("semaphore closed");
    let (mut runners, mut runners_forbidden) = (None, false);
    if want_runners {
        match api::repo_runners(c, &repo.full_name).await {
            Ok(r) => runners = Some(r),
            Err(ApiError::Forbidden { .. } | ApiError::NotFound) => {
                runners = Some(Vec::new());
                runners_forbidden = true;
            }
            Err(e) => return Err(e),
        }
    }
    let mut entries = Vec::new();
    for run in api::active_runs(c, &repo.full_name).await? {
        let jobs = api::run_jobs(c, &run, !repo.private).await?;
        if jobs.is_empty() {
            continue;
        }
        // PR 反查只在這個 run 真的有 self-hosted 或臨時機器的 job 要顯示時才做
        // （整個 run 可能只剩臨時機器的 job，仍要帶 PR 標籤）。這裡只看這個 run 的 runner 名稱形狀；
        // 還在排隊、沒有 runner 名稱的臨時機器 job 認不出來，退回本地標籤。
        let fleet = Fleet::observe(
            jobs.iter().filter_map(|j| j.facts.runner_name.as_deref()),
            jobs.iter()
                .flat_map(|j| j.facts.labels.iter().map(String::as_str)),
        );
        let tag = if jobs.iter().any(|j| {
            needs_self_hosted(&j.facts)
                || j.facts
                    .runner_name
                    .as_deref()
                    .is_some_and(|n| fleet.stem_of(n).is_some())
        }) {
            match api::pr_tag(c, cache, &run).await {
                Ok(t) => t,
                Err(e) if e.is_account_wide() => return Err(e),
                Err(_) => api::local_tag(&run),
            }
        } else {
            api::local_tag(&run)
        };
        entries.push(RunEntry { tag, jobs });
    }
    Ok(RepoScan {
        runners,
        runners_forbidden,
        runs: entries,
    })
}

#[allow(clippy::too_many_arguments)]
async fn scan_scope(
    ctx: &Ctx<'_>,
    name: &str,
    kind: ScopeKind,
    repos: &[RepoInfo],
    rt: &mut ScopeRuntime,
    env: &TickEnv,
    last_cold_scan: i64,
    history: &mut JobHistory,
) -> Result<ScopeView, ApiError> {
    let (c, now, account_id) = (ctx.client, env.now, ctx.account_id);
    let sweep = is_sweep(env.mode, now, last_cold_scan);

    // 名冊
    match kind {
        ScopeKind::Org => {
            // 權限不足的判定只在掃全部的窗口重試，免得每一輪都白打一個 403
            if rt.roster != Some(RosterLevel::BusyOnly) || sweep {
                match api::org_runners(c, name).await {
                    Ok(rs) => {
                        rt.roster = Some(RosterLevel::Full);
                        rt.org_runners = rs;
                    }
                    Err(ApiError::Forbidden { .. } | ApiError::NotFound) => {
                        rt.roster = Some(RosterLevel::BusyOnly);
                        rt.org_runners.clear();
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        ScopeKind::User => rt.roster = Some(RosterLevel::Full),
    }
    let roster = rt.roster.unwrap_or(RosterLevel::Full);

    // 熱冷分層後掃 repo
    let last_active: BTreeSet<String> = rt
        .repo_runs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, _)| k.clone())
        .collect();
    let selected: BTreeSet<String> =
        select_repos(repos, &last_active, last_cold_scan, now, env.mode)
            .into_iter()
            .collect();
    let targets: Vec<&RepoInfo> = repos
        .iter()
        .filter(|r| selected.contains(&r.full_name))
        .collect();
    let want = |r: &RepoInfo| {
        kind == ScopeKind::User && (sweep || !rt.no_runner_access.contains(&r.full_name))
    };
    let wants: Vec<bool> = targets.iter().map(|r| want(r)).collect();
    let results = join_all(
        targets
            .iter()
            .zip(&wants)
            .map(|(r, w)| scan_repo(ctx, r, *w)),
    )
    .await;
    for (repo, res) in targets.iter().zip(results) {
        let scan = match res {
            Ok(scan) => scan,
            Err(e) if e.is_account_wide() => return Err(e),
            // 這個 repo 這輪取不到（例如停用了 Actions）：沿用上一次的事實
            Err(_) => continue,
        };
        // 這輪沒去取名冊（runners 為 None）就不動「有沒有權限」的記憶
        if let Some(rs) = scan.runners {
            rt.repo_runners.insert(repo.full_name.clone(), rs);
            if scan.runners_forbidden {
                rt.no_runner_access.insert(repo.full_name.clone());
            } else {
                rt.no_runner_access.remove(&repo.full_name);
            }
        }
        rt.repo_runs.insert(repo.full_name.clone(), scan.runs);
    }
    let names: BTreeSet<&str> = repos.iter().map(|r| r.full_name.as_str()).collect();
    rt.repo_runs.retain(|k, _| names.contains(k.as_str()));
    rt.repo_runners.retain(|k, _| names.contains(k.as_str()));
    rt.no_runner_access.retain(|k| names.contains(k.as_str()));

    // runner group（低頻）
    if kind == ScopeKind::Org && roster == RosterLevel::Full {
        let stale = rt
            .groups
            .as_ref()
            .is_none_or(|(t, _)| now - *t >= GROUP_INTERVAL_SECS);
        if stale {
            match api::org_runner_groups(c, name).await {
                Ok(g) => rt.groups = Some((now, g)),
                Err(ApiError::Forbidden { .. } | ApiError::NotFound) => {
                    rt.groups = Some((now, Vec::new()))
                }
                Err(e) => return Err(e),
            }
        }
    }

    // 組出 facts
    let mut recs: Vec<RunnerRec> = Vec::new();
    match kind {
        ScopeKind::Org => {
            let groups: &[GroupInfo] = rt.groups.as_ref().map_or(&[], |(_, g)| g.as_slice());
            for raw in &rt.org_runners {
                let g = groups.iter().find(|g| g.runner_ids.contains(&raw.id));
                recs.push(RunnerRec::from_raw(
                    account_id,
                    name,
                    true,
                    raw,
                    g.map_or(GroupAccess::Unknown, |g| g.access.clone()),
                    g.is_some_and(|g| g.restricted_to_workflows),
                    g.map(|g| g.name.clone()),
                ));
            }
        }
        ScopeKind::User => {
            for (repo, raws) in &rt.repo_runners {
                for raw in raws {
                    // repo 層級 runner 只服務自己的 repo
                    let access = GroupAccess::Selected {
                        repos: BTreeSet::from([repo.clone()]),
                        public_ok: true,
                    };
                    recs.push(RunnerRec::from_raw(
                        account_id, repo, false, raw, access, false, None,
                    ));
                }
            }
        }
    }
    // 預估剩餘時間：只為「正在 self-hosted runner 上跑」的 job 查歷史；配額偏低時整個跳過
    let workflow_key = |j: &api::JobInfo| -> Option<WorkflowKey> {
        j.workflow_id.map(|w| (j.facts.repo.clone(), w))
    };
    // 臨時機器的 job 不預估（組裝時會丟掉），所以也不該花歷史查詢的配額去抓它們的樣本
    let all_jobs = || rt.repo_runs.values().flatten().flat_map(|e| &e.jobs);
    let fleet = Fleet::observe(
        recs.iter()
            .map(|r| r.facts.name.as_str())
            .chain(all_jobs().filter_map(|j| j.facts.runner_name.as_deref())),
        all_jobs().flat_map(|j| j.facts.labels.iter().map(String::as_str)),
    );
    let wanted: BTreeSet<WorkflowKey> = all_jobs()
        .filter(|j| wants_history(j, &fleet))
        .filter_map(workflow_key)
        .collect();
    if !wanted.is_empty() && !c.rate().is_low() {
        history.refresh(c, &wanted, now).await;
    }
    let jobs: Vec<JobEntry> = rt
        .repo_runs
        .values()
        .flatten()
        .flat_map(|e| {
            e.jobs.iter().map(|j| JobEntry {
                job: j.clone(),
                tag: e.tag.clone(),
                typical_secs: workflow_key(j)
                    .and_then(|k| history.typical(&k, &j.job_name, j.facts.runner_name.as_deref())),
            })
        })
        .collect();

    let out = assemble(
        &ScopeInput {
            account_id,
            name,
            kind,
            roster,
            runners: &recs,
            jobs: &jobs,
            now,
        },
        &rt.state,
        &rt.attention,
        !rt.observed,
    );
    rt.state = out.state;
    rt.attention = out.attention;
    rt.observed = true;
    Ok(out.view)
}

pub struct Collector {
    accounts: Vec<Account>,
    hourly_target: u32,
    now: fn() -> i64,
    last_full_at: Option<i64>,
    /// 最近一次一般輪詢花的計費請求數，用來推估每小時用量
    last_incremental_cost: u64,
    /// 每台 runner 跑過哪些 job（呼叫端負責存檔與啟動時載入）
    runlog: RunLog,
}

impl Collector {
    /// `states` 的 key＝`"{account_id}/{scope}"`（上次存下的觀察狀態，隨 snapshot 保存）。
    pub fn new(
        accounts: Vec<(AccountMeta, GitHubClient)>,
        states: BTreeMap<String, ObservationState>,
        hourly_target: u32,
        now: fn() -> i64,
    ) -> Self {
        let accounts = accounts
            .into_iter()
            .map(|(meta, client)| {
                let prefix = format!("{}/", meta.account_id);
                let scopes = states
                    .iter()
                    .filter_map(|(k, st)| {
                        k.strip_prefix(&prefix).map(|scope| {
                            (
                                scope.to_string(),
                                ScopeRuntime {
                                    state: st.clone(),
                                    ..Default::default()
                                },
                            )
                        })
                    })
                    .collect();
                Account::new(meta, client, scopes)
            })
            .collect();
        Self {
            accounts,
            hourly_target: clamp_hourly_target(hourly_target),
            now,
            last_full_at: None,
            last_incremental_cost: 0,
            runlog: RunLog::default(),
        }
    }

    /// 啟動時載入上次存下的執行歷史。
    pub fn set_runlog(&mut self, runlog: RunLog) {
        self.runlog = runlog;
    }

    pub fn runlog(&self) -> &RunLog {
        &self.runlog
    }

    /// 記下這一輪正在跑的 job，並替已經結束的 job 查最終結果。
    async fn update_runlog(&mut self, views: &[AccountView], now: i64) {
        self.runlog.observe(views, now);
        // (帳號, job id)：一個帳號暫停時殘留的舊畫面，不能擋住別的帳號查結果
        let active: BTreeSet<(u64, u64)> = views
            .iter()
            .flat_map(|a| {
                a.scopes
                    .iter()
                    .flat_map(|s| &s.runners)
                    .filter_map(|r| r.job.as_ref())
                    .filter_map(|j| j.key.split_once('/').and_then(|(_, id)| id.parse().ok()))
                    .map(move |id| (a.account_id, id))
            })
            .collect();
        let due = self
            .runlog
            .due_for_result(&active, now, MAX_RESULTS_PER_TICK);
        let accounts = &self.accounts;
        let lookups = due
            .into_iter()
            .filter_map(|(key, account_id, repo, job_id)| {
                let a = accounts.iter().find(|a| {
                    a.meta.account_id == account_id && a.hold.is_none() && !a.client.rate().is_low()
                })?;
                Some(async move {
                    let res = api::job_status(&a.client, &repo, job_id).await;
                    (key, job_id, res)
                })
            });
        for (key, job_id, res) in join_all(lookups).await {
            match res {
                Ok(j) => self.runlog.apply_result(&key, job_id, Some(&j), now),
                Err(ApiError::NotFound) => self.runlog.apply_result(&key, job_id, None, now),
                Err(_) => self.runlog.touch(&key, job_id, now),
            }
        }
        let ids: BTreeSet<u64> = self.accounts.iter().map(|a| a.meta.account_id).collect();
        self.runlog.prune(now, &ids);
    }

    /// 「載入更早的紀錄」：查範圍內最近有推送的 repo 的已完成執行，挑出跑在這台 runner 上的 job。
    /// 只在使用者按下時才做；請求數有上限（最多 `BACKFILL_REPOS` ×（1 ＋ `BACKFILL_RUNS_PER_REPO`））。
    pub async fn backfill(
        &mut self,
        account_id: u64,
        scope: &str,
        runner_name: &str,
        runner_key: &str,
    ) -> BackfillReport {
        let now = (self.now)();
        let mut report = BackfillReport::default();
        let Some(a) = self
            .accounts
            .iter()
            .find(|a| a.meta.account_id == account_id)
        else {
            return report;
        };
        // 只服務單一 repo 的 runner 只查那個 repo；org 層級的查範圍內最近有推送的 repo
        let ident = parse_runner_key(runner_key);
        let only_repo = ident.as_ref().and_then(|i| i.repo.clone());
        let runner_id = ident.as_ref().and_then(|i| i.runner_id);
        let mut repos: Vec<&RepoInfo> = a
            .repos
            .iter()
            .filter(|r| match &only_repo {
                Some(repo) => r.full_name.eq_ignore_ascii_case(repo),
                None => r.owner_login == scope && now - r.pushed_at < BACKFILL_PUSHED_WITHIN_SECS,
            })
            .collect();
        repos.sort_by_key(|r| std::cmp::Reverse(r.pushed_at));
        repos.truncate(BACKFILL_REPOS);
        for repo in repos {
            let runs = match api::recent_completed_runs(
                &a.client,
                &repo.full_name,
                BACKFILL_RUNS_PER_REPO,
            )
            .await
            {
                Ok(runs) => runs,
                Err(_) => {
                    report.partial = true;
                    continue;
                }
            };
            report.repos_checked += 1;
            let all = join_all(runs.iter().map(|run| api::run_jobs_all(&a.client, run))).await;
            for (run, res) in runs.iter().zip(all) {
                report.runs_checked += 1;
                let Ok(jobs) = res else {
                    report.partial = true;
                    continue;
                };
                for rec in
                    backfill_records(run, &jobs, runner_id, runner_name, &api::local_tag(run))
                {
                    if self.runlog.insert(runner_key, account_id, rec) {
                        report.added += 1;
                    }
                }
            }
        }
        let ids: BTreeSet<u64> = self.accounts.iter().map(|a| a.meta.account_id).collect();
        self.runlog.prune(now, &ids);
        report
    }

    /// 新增（或以相同 id 取代）一個帳號。
    pub fn add_account(&mut self, meta: AccountMeta, client: GitHubClient) {
        self.accounts
            .retain(|a| a.meta.account_id != meta.account_id);
        self.accounts
            .push(Account::new(meta, client, BTreeMap::new()));
    }

    /// 移除帳號與它所有 scope 的狀態。
    pub fn remove_account(&mut self, account_id: u64) {
        self.accounts.retain(|a| a.meta.account_id != account_id);
    }

    /// 改勾選的 scope。下一輪就會重新探索 repo 清單，新勾選的 scope 立刻有資料。
    pub fn set_scopes(&mut self, account_id: u64, selected: BTreeSet<String>) {
        if let Some(a) = self
            .accounts
            .iter_mut()
            .find(|a| a.meta.account_id == account_id)
        {
            a.meta.selected_scopes = selected;
            a.last_cold_scan = 0;
        }
    }

    /// token 續期後換上新的 access token（之後的請求都用它）。
    pub fn set_token(&self, account_id: u64, access_token: String) {
        if let Some(a) = self
            .accounts
            .iter()
            .find(|a| a.meta.account_id == account_id)
        {
            a.client.set_token(access_token);
        }
    }

    /// 暫停掃描某帳號並顯示對應狀態；`None` 解除。
    pub fn hold(&mut self, account_id: u64, hold: Option<Hold>) {
        if let Some(a) = self
            .accounts
            .iter_mut()
            .find(|a| a.meta.account_id == account_id)
        {
            a.hold = hold;
        }
    }

    /// 目前所有帳號的設定（給呼叫端存檔）。
    pub fn accounts(&self) -> Vec<AccountMeta> {
        self.accounts.iter().map(|a| a.meta.clone()).collect()
    }

    /// 下一輪生效。
    pub fn set_hourly_target(&mut self, n: u32) {
        self.hourly_target = clamp_hourly_target(n);
    }

    pub fn hourly_target(&self) -> u32 {
        self.hourly_target
    }

    /// 要隨 snapshot 存到磁碟的觀察狀態。
    pub fn states(&self) -> BTreeMap<String, ObservationState> {
        self.accounts
            .iter()
            .flat_map(|a| {
                a.scopes
                    .iter()
                    .map(|(scope, rt)| (format!("{}/{scope}", a.meta.account_id), rt.state.clone()))
            })
            .collect()
    }

    /// Android 回到前景：下一輪每個 scope 都視為「第一次看到」。
    pub fn resume(&mut self) {
        for rt in self.accounts.iter_mut().flat_map(|a| a.scopes.values_mut()) {
            rt.observed = false;
        }
    }

    pub async fn tick(&mut self, mode: TickMode) -> Snapshot {
        let now = (self.now)();
        let effective = if mode == TickMode::Full
            && self
                .last_full_at
                .is_none_or(|t| now - t >= FULL_SCAN_MIN_GAP_SECS)
        {
            TickMode::Full
        } else {
            TickMode::Incremental
        };
        if effective == TickMode::Full {
            self.last_full_at = Some(now);
        }
        let billed = |accounts: &[Account]| -> u64 {
            accounts.iter().map(|a| a.client.billed_total()).sum()
        };
        let before = billed(&self.accounts);

        let env = TickEnv {
            now,
            mode: effective,
        };
        let views = join_all(self.accounts.iter_mut().map(|a| a.run(&env))).await;

        // 執行歷史的結果查詢也算進這一輪的成本（額度控制會一併考慮）
        self.update_runlog(&views, now).await;
        if effective == TickMode::Incremental {
            self.last_incremental_cost = billed(&self.accounts).saturating_sub(before);
        }
        let summary = summarize(&views);
        Snapshot {
            schema_version: SCHEMA_VERSION,
            generated_at: now,
            accounts: views,
            summary,
        }
    }

    /// 這一輪之後等多久再輪詢。真的被限流（收到 403／429）的帳號由用戶端擋住提前重試；
    /// 全部帳號都被限流時，等到最早的解除時間，不受 600 秒上限截短。
    /// 只是配額偏低（< 20%）不算被限流：那只拉長間隔。
    pub fn next_delay(&self, snap: &Snapshot) -> Duration {
        let has_work =
            snap.summary.own_busy + snap.summary.pool_busy > 0 || snap.summary.waiting > 0;
        let base = base_delay_secs(has_work);
        let low_quota = self.accounts.iter().any(|a| a.client.rate().is_low());
        let projected = (self.last_incremental_cost.saturating_mul(3_600) / base)
            .min(u64::from(u32::MAX)) as u32;
        let mut delay = compute_delay(&DelayInputs {
            has_work,
            low_quota,
            projected_per_hour: projected,
            hourly_target: self.hourly_target,
        });
        let blocked: Vec<i64> = self
            .accounts
            .iter()
            .filter_map(|a| a.client.blocked_until())
            .collect();
        if !self.accounts.is_empty() && blocked.len() == self.accounts.len() {
            let wait = blocked.into_iter().min().unwrap_or(0) - (self.now)();
            delay = delay.max(Duration::from_secs(wait.max(0) as u64));
        }
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str, pushed_at: i64) -> RepoInfo {
        RepoInfo {
            full_name: name.into(),
            owner_login: "o".into(),
            owner_kind: ScopeKind::Org,
            pushed_at,
            private: true,
        }
    }
    const NOW: i64 = 1_000_000;
    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn job_on(runner: &str, group: &str) -> JobInfo {
        JobInfo {
            facts: pr_runners_rules::JobFacts {
                key: "1/1".into(),
                repo: "o/r".into(),
                repo_public: false,
                state: pr_runners_rules::JobState::InProgress,
                labels: vec![],
                runner_id: Some(1),
                runner_name: Some(runner.into()),
                runner_group_name: Some(group.into()),
                created_at: 0,
                started_at: Some(1),
            },
            run_id: 1,
            job_id: 1,
            workflow_id: Some(7),
            workflow_name: "ci".into(),
            job_name: "t".into(),
            html_url: String::new(),
            run_attempt: 1,
        }
    }

    #[test]
    fn history_is_only_fetched_for_jobs_on_own_runners() {
        // 臨時機器上的 job（group 非空，needs_self_hosted 為 true）不查歷史；自家機器的要查
        let pool = job_on("pool-8x-linux-q7k2m9x4b1c8d3e5", "Default");
        let own = job_on("netcup", "Default");
        let fleet = Fleet::observe(["pool-8x-linux-q7k2m9x4b1c8d3e5", "netcup"], []);
        assert!(
            needs_self_hosted(&pool.facts),
            "前提：它會被當成 self-hosted"
        );
        assert!(!wants_history(&pool, &fleet));
        assert!(wants_history(&own, &fleet));
    }

    #[test]
    fn full_mode_selects_all() {
        let repos = [repo("o/a", NOW), repo("o/b", 0)];
        let got = select_repos(&repos, &set(&[]), NOW - 10, NOW, TickMode::Full);
        assert_eq!(got, ["o/a", "o/b"]);
    }

    #[test]
    fn incremental_selects_hot_only_before_cold_window() {
        let repos = [
            repo("o/pushed-recently", NOW - 3_600),
            repo("o/was-active", 0),
            repo("o/cold", NOW - 2 * HOT_WINDOW_SECS),
            repo("o/edge-hot", NOW - HOT_WINDOW_SECS),
            repo("o/edge-cold", NOW - HOT_WINDOW_SECS - 1),
        ];
        let got = select_repos(
            &repos,
            &set(&["o/was-active"]),
            NOW - 599,
            NOW,
            TickMode::Incremental,
        );
        assert_eq!(got, ["o/pushed-recently", "o/was-active", "o/edge-hot"]);
    }

    #[test]
    fn cold_repos_included_every_600s() {
        let repos = [repo("o/hot", NOW), repo("o/cold", 0)];
        let got = select_repos(&repos, &set(&[]), NOW - 600, NOW, TickMode::Incremental);
        assert_eq!(got, ["o/hot", "o/cold"]);
    }

    fn delay(has_work: bool, low_quota: bool, projected: u32, target: u32) -> u64 {
        compute_delay(&DelayInputs {
            has_work,
            low_quota,
            projected_per_hour: projected,
            hourly_target: target,
        })
        .as_secs()
    }

    #[test]
    fn delay_30_when_busy_120_when_idle() {
        assert_eq!(delay(true, false, 0, 2_500), 30);
        assert_eq!(delay(false, false, 0, 2_500), 120);
    }

    #[test]
    fn delay_doubles_when_remaining_below_20_percent() {
        assert_eq!(delay(true, true, 0, 2_500), 60);
        assert_eq!(delay(false, true, 0, 2_500), 240);
    }

    #[test]
    fn delay_doubles_when_projected_over_target() {
        assert_eq!(delay(true, false, 2_600, 2_500), 60, "推估 2,600 > 2,500");
        assert_eq!(delay(true, false, 2_400, 2_500), 30, "推估 2,400 不變");
        assert_eq!(delay(true, false, 2_500, 2_500), 30, "剛好等於目標不算超過");
        assert_eq!(
            delay(true, false, 2_600, 3_000),
            30,
            "目標調到 3,000 後 2,600 不變"
        );
    }

    #[test]
    fn over_budget_backs_off_progressively_until_the_cap() {
        // 以 30 秒間隔推估每小時用量；間隔每加倍，用量減半，直到不超過目標
        assert_eq!(delay(true, false, 5_000, 2_500), 60);
        assert_eq!(delay(true, false, 5_001, 2_500), 120);
        assert_eq!(delay(true, false, 20_000, 2_500), 240);
        assert_eq!(delay(true, false, 40_000, 2_500), 480);
        // 閒置：120 → 240 → 480 → 600
        assert_eq!(delay(false, false, 10_000, 2_500), 480);
    }

    #[test]
    fn delay_never_exceeds_600() {
        assert_eq!(delay(true, false, u32::MAX, 500), 600);
        assert_eq!(delay(false, false, u32::MAX, 500), 600);
        assert_eq!(delay(false, true, u32::MAX, 500), 600);
        assert_eq!(delay(false, true, 0, 2_500), 240, "配額低：只多乘 2");
        assert_eq!(
            delay(false, true, 10_001, 2_500),
            600,
            "480 再乘 2 也被上限截住"
        );
    }

    #[test]
    fn set_hourly_target_clamps() {
        let mut c = Collector::new(Vec::new(), BTreeMap::new(), 2_500, || 0);
        c.set_hourly_target(9_999);
        assert_eq!(c.hourly_target(), 4_500);
        c.set_hourly_target(100);
        assert_eq!(c.hourly_target(), 500);
        c.set_hourly_target(3_000);
        assert_eq!(c.hourly_target(), 3_000);
    }
}
