//! 把「一個 scope 的完整事實」組成 `ScopeView`：job 對上 runner、補出名冊裡沒有的忙碌 runner、
//! 套用「需要注意」規則、算排隊對位。純函式，不碰網路。
//!
//! Review Focus 2：runner 以 `{account}/{註冊 scope}/{runner id}` 識別。job 只能對到
//! 「org 層級 runner」或「註冊在自己 repo 的 repo 層級 runner」，所以兩個 org 各有一台
//! 同名 `builder-1` 也不會互相錯配。

use crate::api::{JobInfo, RawRunner, ScopeKind};
use crate::view::*;
use pr_runners_rules::{
    Attention, AttentionEvent, Candidate, CandidateKind, Fleet, GroupAccess, JobFacts, JobObs,
    JobState, ObservationState, PoolTag, RunnerFacts, RunnerObs, candidates, elapsed, evaluate,
    needs_self_hosted, pool_tag, queued_age,
};
use std::collections::BTreeMap;

pub fn runner_key(account_id: u64, scope: &str, runner_id: u64) -> String {
    format!("{account_id}/{scope}/{runner_id}")
}

/// 從 runner key 解回它的身分（`runner_key` 與權限不足時降級 key 的反向；key 的格式只在這裡定義）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerIdentity {
    /// org 名稱，或 repo 層級 runner 的 repo 全名
    pub scope: String,
    pub runner_id: Option<u64>,
    /// 只服務單一 repo 的 runner（repo 層級，或權限不足時降級識別的）
    pub repo: Option<String>,
    /// 沒有 id 時用來識別的名稱
    pub name: Option<String>,
}

pub fn parse_runner_key(key: &str) -> Option<RunnerIdentity> {
    let (account, rest) = key.split_once('/')?;
    account.parse::<u64>().ok()?;
    // 權限不足的降級 key：`{account}/{scope}/{owner}/{repo}:{name}`（repo 名稱不會有冒號）
    if let Some((left, name)) = rest.split_once(':') {
        let (scope, repo) = left.split_once('/')?;
        if !repo.contains('/') {
            return None;
        }
        return Some(RunnerIdentity {
            scope: scope.into(),
            runner_id: None,
            repo: Some(repo.into()),
            name: Some(name.into()),
        });
    }
    let (scope, id) = rest.rsplit_once('/')?;
    Some(RunnerIdentity {
        scope: scope.into(),
        runner_id: Some(id.parse().ok()?),
        repo: scope.contains('/').then(|| scope.to_string()),
        name: None,
    })
}

/// 名冊上的一台 runner（或從 job 反推補出來的）。
#[derive(Debug, Clone)]
pub struct RunnerRec {
    pub facts: RunnerFacts,
    pub os: String,
    pub group_name: Option<String>,
    /// 註冊所在：org 名稱，或 repo 全名
    pub reg_scope: String,
    /// org 層級的 runner 可以服務 org 內任何 repo；repo 層級的只服務自己那個 repo
    pub org_level: bool,
}

/// 一個 active job，加上它所屬 run 的 PR 標籤。
#[derive(Debug, Clone)]
pub struct JobEntry {
    pub job: JobInfo,
    pub tag: String,
    /// 平常跑多久（秒），由收集器依歷次執行算好；沒有就不預估
    pub typical_secs: Option<u64>,
}

impl RunnerRec {
    /// 名冊上的一台 runner（org 層級或 repo 層級）。`reg_scope` 是註冊所在（org 名稱或 repo 全名）。
    pub fn from_raw(
        account_id: u64,
        reg_scope: &str,
        org_level: bool,
        raw: &RawRunner,
        group_access: GroupAccess,
        restricted_to_workflows: bool,
        group_name: Option<String>,
    ) -> Self {
        Self {
            facts: RunnerFacts {
                key: runner_key(account_id, reg_scope, raw.id),
                id: raw.id,
                name: raw.name.clone(),
                online: raw.online,
                busy: raw.busy,
                labels: raw.labels.clone(),
                group_access,
                restricted_to_workflows,
            },
            os: raw.os.clone(),
            group_name,
            reg_scope: reg_scope.to_string(),
            org_level,
        }
    }
}

pub struct ScopeInput<'a> {
    pub account_id: u64,
    pub name: &'a str,
    pub kind: ScopeKind,
    pub roster: RosterLevel,
    pub runners: &'a [RunnerRec],
    pub jobs: &'a [JobEntry],
    pub now: i64,
}

pub struct Assembled {
    pub view: ScopeView,
    pub state: ObservationState,
    pub attention: Vec<Attention>,
    pub events: Vec<AttentionEvent>,
}

fn synth(
    key: String,
    id: u64,
    name: String,
    labels: Vec<String>,
    scope: &str,
    org_level: bool,
) -> RunnerRec {
    RunnerRec {
        facts: RunnerFacts {
            key,
            id,
            name,
            online: true,
            busy: true,
            labels,
            group_access: GroupAccess::Unknown,
            restricted_to_workflows: false,
        },
        os: String::new(),
        group_name: None,
        reg_scope: scope.to_string(),
        org_level,
    }
}

/// 這個進行中的 self-hosted job 跑在哪一列 runner 上；名冊沒有就補一列忙碌。
fn runner_for_job(recs: &mut Vec<RunnerRec>, input: &ScopeInput, job: &JobInfo) -> Option<usize> {
    let f = &job.facts;
    let name = f.runner_name.clone();
    match f.runner_id {
        Some(id) => {
            if let Some(i) = recs.iter().position(|r| {
                r.facts.id == id && (r.org_level || r.reg_scope.eq_ignore_ascii_case(&f.repo))
            }) {
                return Some(i);
            }
            // 名冊裡沒有：完整名冊時多半是 repo 層級 runner（org 名冊看不到）；
            // 權限不足時整份名冊就是這樣反推出來的。
            let (scope, org_level) = match input.roster {
                RosterLevel::Full => (f.repo.as_str(), false),
                RosterLevel::BusyOnly => (input.name, true),
            };
            let name = name.unwrap_or_else(|| format!("runner {id}"));
            recs.push(synth(
                runner_key(input.account_id, scope, id),
                id,
                name,
                f.labels.clone(),
                scope,
                org_level,
            ));
            Some(recs.len() - 1)
        }
        // 沒有可用的 id：只有權限不足時才以「repo ＋ 非空名稱」降級識別，不跨 repo 合併。
        None => match (input.roster, name) {
            (RosterLevel::BusyOnly, Some(name)) => {
                let key = format!("{}/{}/{}:{}", input.account_id, input.name, f.repo, name);
                if let Some(i) = recs.iter().position(|r| r.facts.key == key) {
                    return Some(i);
                }
                recs.push(synth(key, 0, name, f.labels.clone(), &f.repo, false));
                Some(recs.len() - 1)
            }
            _ => None,
        },
    }
}

/// 這個 job 跑在（或在等）哪一組臨時機器：已經在跑的看 runner 名稱，排隊中的看 labels。
fn job_pool(fleet: &Fleet, f: &JobFacts) -> Option<PoolTag> {
    let stem = match f.runner_name.as_deref() {
        Some(name) => fleet.stem_of(name),
        None => fleet.waits_for(&f.labels),
    };
    stem.map(|s| pool_tag(&s))
}

fn job_view(e: &JobEntry, fleet: &Fleet) -> JobView {
    let f = &e.job.facts;
    let pool = job_pool(fleet, f);
    JobView {
        key: f.key.clone(),
        repo: f.repo.clone(),
        workflow: e.job.workflow_name.clone(),
        job: e.job.job_name.clone(),
        state: f.state.clone(),
        labels: f.labels.clone(),
        pr_tag: e.tag.clone(),
        url: e.job.html_url.clone(),
        runner_name: f.runner_name.clone(),
        hosted: !needs_self_hosted(f) && pool.is_none(),
        // 臨時機器的 job 不預估：歷史只記自家 runner 的樣本，拿它預估雲端 VM 會誤導
        typical_secs: if pool.is_some() { None } else { e.typical_secs },
        pool,
    }
}

pub fn assemble(
    input: &ScopeInput,
    prev: &ObservationState,
    prev_attention: &[Attention],
    first_observation: bool,
) -> Assembled {
    let now = input.now;
    let mut recs: Vec<RunnerRec> = input.runners.to_vec();

    // job → runner（只看進行中的 self-hosted job；hosted 的 `GitHub Actions 3` 不進名冊）
    let mut job_on: BTreeMap<usize, &JobEntry> = BTreeMap::new();
    for e in input
        .jobs
        .iter()
        .filter(|e| e.job.facts.state == JobState::InProgress && needs_self_hosted(&e.job.facts))
    {
        if let Some(i) = runner_for_job(&mut recs, input, &e.job) {
            job_on.entry(i).or_insert(e);
            recs[i].facts.busy = recs[i].facts.online;
        }
    }

    // 臨時機器分類：名冊（含補出來的忙碌列）與 job 上的 runner 名稱看名稱形狀，
    // 這個 scope 全部 job 的 labels 提供「組鍵等於 job label」那條規則
    let fleet = Fleet::observe(
        recs.iter().map(|r| r.facts.name.as_str()).chain(
            input
                .jobs
                .iter()
                .filter_map(|e| e.job.facts.runner_name.as_deref()),
        ),
        input
            .jobs
            .iter()
            .flat_map(|e| e.job.facts.labels.iter().map(String::as_str)),
    );
    let job_pools: Vec<Option<PoolTag>> = input
        .jobs
        .iter()
        .map(|e| job_pool(&fleet, &e.job.facts))
        .collect();

    // 需要注意
    let runner_obs: Vec<RunnerObs> = recs
        .iter()
        .map(|r| RunnerObs {
            key: r.facts.key.clone(),
            online: r.facts.online,
        })
        .collect();
    // 等臨時機器的 job 也算「等自家資源」：排隊超過 10 分鐘一樣會被 flag
    let job_obs: Vec<JobObs> = input
        .jobs
        .iter()
        .zip(&job_pools)
        .map(|(e, pool)| JobObs {
            key: e.job.facts.key.clone(),
            state: e.job.facts.state.clone(),
            created_at: e.job.facts.created_at,
            self_hosted: needs_self_hosted(&e.job.facts) || pool.is_some(),
        })
        .collect();
    let (state, attention, events) = evaluate(
        prev,
        prev_attention,
        &runner_obs,
        &job_obs,
        now,
        first_observation,
    );
    let job_attn = |key: &str| {
        attention
            .iter()
            .any(|a| matches!(a, Attention::JobQueuedTooLong { key: k, .. } if k == key))
    };

    // 排隊對位：只對排隊中的 self-hosted job；runner 只傳同一個 scope 的
    let facts: Vec<RunnerFacts> = recs.iter().map(|r| r.facts.clone()).collect();
    let mut queued: Vec<(&JobEntry, Option<&PoolTag>)> = input
        .jobs
        .iter()
        .zip(&job_pools)
        .filter(|(e, pool)| {
            e.job.facts.state == JobState::Queued
                && (needs_self_hosted(&e.job.facts) || pool.is_some())
        })
        .map(|(e, pool)| (e, pool.as_ref()))
        .collect();
    queued.sort_by(|(a, _), (b, _)| {
        (a.job.facts.created_at, &a.job.facts.key).cmp(&(b.job.facts.created_at, &b.job.facts.key))
    });
    let mut can_take: BTreeMap<String, CandidateKind> = BTreeMap::new();
    let waiting: Vec<WaitingView> = queued
        .iter()
        .map(|(e, pool)| {
            let f = &e.job.facts;
            // 等臨時機器的 job 沒有「哪台自家 runner 能接」的問題：不對位
            let cands: Vec<Candidate> = if pool.is_some() {
                Vec::new()
            } else {
                candidates(f, &facts)
            };
            for c in &cands {
                can_take
                    .entry(c.runner_key.clone())
                    .and_modify(|k| *k = (*k).min(c.kind))
                    .or_insert(c.kind);
            }
            WaitingView {
                job: job_view(e, &fleet),
                age: queued_age(now, f.created_at),
                attention: job_attn(&f.key),
                candidates: cands,
            }
        })
        .collect();

    let mut runners: Vec<RunnerView> = recs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let key = &r.facts.key;
            let job = job_on.get(&i).copied();
            let rstate = if !r.facts.online {
                RunnerState::Offline {
                    since: state
                        .offline_since
                        .get(key)
                        .copied()
                        .unwrap_or(pr_runners_rules::OfflineSince::Unknown),
                }
            } else if r.facts.busy {
                RunnerState::Busy
            } else {
                RunnerState::Idle
            };
            let el = match (&rstate, job) {
                (RunnerState::Busy, Some(e)) => e
                    .job
                    .facts
                    .started_at
                    .map(|s| elapsed(now, e.job.facts.created_at, s)),
                _ => None,
            };
            RunnerView {
                key: key.clone(),
                name: r.facts.name.clone(),
                os: r.os.clone(),
                state: rstate,
                labels: r.facts.labels.clone(),
                group: r.group_name.clone(),
                job: job.map(|e| job_view(e, &fleet)),
                elapsed: el,
                pool: fleet.stem_of(&r.facts.name).map(|s| pool_tag(&s)),
                can_take: can_take.get(key).copied(),
            }
        })
        .collect();
    runners.sort_by(|a, b| (&a.name, &a.key).cmp(&(&b.name, &b.key)));

    let mut jobs: Vec<JobView> = input.jobs.iter().map(|e| job_view(e, &fleet)).collect();
    jobs.sort_by(|a, b| a.key.cmp(&b.key));

    Assembled {
        view: ScopeView {
            name: input.name.to_string(),
            kind: input.kind,
            roster: input.roster,
            runners,
            waiting,
            jobs,
            last_updated_at: now,
        },
        state,
        attention,
        events,
    }
}

/// 全部帳號、全部 scope 的計數與嚴重度。自家機器與臨時機器分開算；離線的臨時機器殘骸不進任何計數。
pub fn summarize(accounts: &[AccountView]) -> Summary {
    let mut s = Summary {
        own_busy: 0,
        own_online: 0,
        own_offline: 0,
        pool_busy: 0,
        waiting: 0,
        attention: Severity::Ok,
        stale: accounts.iter().any(|a| {
            matches!(
                a.status,
                AccountStatus::Offline { .. } | AccountStatus::Throttled { .. }
            )
        }),
    };
    for scope in accounts.iter().flat_map(|a| a.scopes.iter()) {
        for r in &scope.runners {
            match (&r.pool, &r.state) {
                (None, RunnerState::Busy) => {
                    s.own_busy += 1;
                    s.own_online += 1;
                }
                (None, RunnerState::Idle) => s.own_online += 1,
                (None, RunnerState::Offline { .. }) => s.own_offline += 1,
                (Some(_), RunnerState::Busy) => s.pool_busy += 1,
                (Some(_), _) => {}
            }
        }
        s.waiting += scope.waiting.len() as u32;
        if scope.waiting.iter().any(|w| w.attention) {
            s.attention = Severity::Warn;
        }
    }
    s
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn runner_keys_roundtrip_to_their_identity() {
        let org = runner_key(1, "Acme", 22);
        assert_eq!(
            parse_runner_key(&org),
            Some(RunnerIdentity {
                scope: "Acme".into(),
                runner_id: Some(22),
                repo: None,
                name: None
            })
        );
        let repo_level = runner_key(1, "me/proj", 7);
        assert_eq!(
            parse_runner_key(&repo_level),
            Some(RunnerIdentity {
                scope: "me/proj".into(),
                runner_id: Some(7),
                repo: Some("me/proj".into()),
                name: None
            })
        );
        assert_eq!(
            parse_runner_key("1/Acme/Acme/r:builder-1"),
            Some(RunnerIdentity {
                scope: "Acme".into(),
                runner_id: None,
                repo: Some("Acme/r".into()),
                name: Some("builder-1".into())
            })
        );
        assert_eq!(parse_runner_key("garbage"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pr_runners_rules::JobFacts;

    fn rec(scope: &str, id: u64, name: &str) -> RunnerRec {
        RunnerRec {
            facts: RunnerFacts {
                key: runner_key(1, scope, id),
                id,
                name: name.into(),
                online: true,
                busy: false,
                labels: vec!["self-hosted".into()],
                group_access: GroupAccess::Unknown,
                restricted_to_workflows: false,
            },
            os: String::new(),
            group_name: None,
            reg_scope: scope.into(),
            org_level: false,
        }
    }
    fn running(repo: &str, runner_id: u64) -> JobEntry {
        JobEntry {
            job: JobInfo {
                facts: JobFacts {
                    key: "1/1".into(),
                    repo: repo.into(),
                    repo_public: false,
                    state: JobState::InProgress,
                    labels: vec!["self-hosted".into()],
                    runner_id: Some(runner_id),
                    runner_name: Some("x".into()),
                    runner_group_name: Some("Default".into()),
                    created_at: 0,
                    started_at: Some(10),
                },
                run_id: 1,
                job_id: 1,
                workflow_id: None,
                workflow_name: "ci".into(),
                job_name: "t".into(),
                html_url: String::new(),
                run_attempt: 1,
            },
            tag: "PR#1".into(),
            typical_secs: None,
        }
    }

    #[test]
    fn repo_level_runner_only_matches_jobs_of_its_own_repo() {
        // 個人帳號兩個 repo 各有一台 runner，id 相同：repo b 的 job 只能對到 b 的那台
        let runners = [rec("u/a", 5, "a-box"), rec("u/b", 5, "b-box")];
        let jobs = [running("u/b", 5)];
        let input = ScopeInput {
            account_id: 1,
            name: "u",
            kind: ScopeKind::User,
            roster: RosterLevel::Full,
            runners: &runners,
            jobs: &jobs,
            now: 100,
        };
        let out = assemble(&input, &ObservationState::default(), &[], true);
        let state_of = |n: &str| {
            out.view
                .runners
                .iter()
                .find(|r| r.name == n)
                .unwrap()
                .state
                .clone()
        };
        assert_eq!(state_of("b-box"), RunnerState::Busy);
        assert_eq!(state_of("a-box"), RunnerState::Idle);
        assert_eq!(out.view.runners.len(), 2, "沒有多補一列");
    }

    #[test]
    fn running_job_on_runner_missing_from_full_roster_adds_a_busy_row() {
        // 完整名冊裡沒有的 runner（例如 org 內某 repo 自己的 repo 層級 runner）：補一列忙碌
        let jobs = [running("o/solo", 77)];
        let input = ScopeInput {
            account_id: 1,
            name: "o",
            kind: ScopeKind::Org,
            roster: RosterLevel::Full,
            runners: &[],
            jobs: &jobs,
            now: 100,
        };
        let out = assemble(&input, &ObservationState::default(), &[], true);
        assert_eq!(out.view.runners.len(), 1);
        let r = &out.view.runners[0];
        assert_eq!(
            (r.state.clone(), r.group.clone()),
            (RunnerState::Busy, None)
        );
        assert_eq!(r.key, "1/o/solo/77");
        assert_eq!(
            r.elapsed,
            Some(pr_runners_rules::Elapsed::Known { secs: 90 })
        );
    }

    fn pool_job(key: &str, state: JobState, runner: Option<(u64, &str)>) -> JobEntry {
        let mut e = running("o/r", 5);
        e.job.facts.key = key.into();
        e.job.facts.state = state;
        e.job.facts.labels = vec!["pool-8x-linux".into()];
        e.job.facts.runner_id = runner.map(|r| r.0);
        e.job.facts.runner_name = runner.map(|r| r.1.to_string());
        e
    }

    fn org_input<'a>(
        roster: RosterLevel,
        runners: &'a [RunnerRec],
        jobs: &'a [JobEntry],
        now: i64,
    ) -> ScopeInput<'a> {
        ScopeInput {
            account_id: 1,
            name: "o",
            kind: ScopeKind::Org,
            roster,
            runners,
            jobs,
            now,
        }
    }

    fn account(status: AccountStatus, scope: ScopeView) -> AccountView {
        AccountView {
            account_id: 1,
            login: "u".into(),
            status,
            scopes: vec![scope],
        }
    }

    fn pool_rec(id: u64, tail: &str, online: bool, busy: bool) -> RunnerRec {
        let mut r = rec("o", id, &format!("pool-8x-linux-{tail}"));
        r.facts.online = online;
        r.facts.busy = busy;
        r.facts.labels = vec![];
        r
    }

    #[test]
    fn pool_runners_are_tagged_and_offline_leftovers_do_not_count() {
        let mut own_busy = rec("o", 1, "netcup");
        own_busy.facts.busy = true;
        let mut own_off = rec("o", 2, "la10");
        own_off.facts.online = false;
        let runners = [
            own_busy,
            own_off,
            pool_rec(10, "q7k2m9x4b1c8d3e5", true, true),
            pool_rec(11, "z8y7x6w5v4u3t2s1", false, false),
            pool_rec(12, "a1b2c3d4e5f6g7h8", false, false),
        ];
        let out = assemble(
            &org_input(RosterLevel::Full, &runners, &[], 100),
            &ObservationState::default(),
            &[],
            true,
        );
        let pool_of = |n: &str| {
            out.view
                .runners
                .iter()
                .find(|r| r.name == n)
                .unwrap()
                .pool
                .clone()
        };
        assert_eq!(pool_of("netcup"), None);
        let tag = pool_of("pool-8x-linux-q7k2m9x4b1c8d3e5").expect("pool runner is tagged");
        assert_eq!(
            (tag.name.as_str(), tag.machine.as_str()),
            ("pool", "8x-linux")
        );
        let s = summarize(&[account(AccountStatus::Ok, out.view)]);
        assert_eq!(
            (
                s.own_busy,
                s.own_online,
                s.own_offline,
                s.pool_busy,
                s.attention
            ),
            (1, 1, 1, 1, Severity::Ok)
        );
    }

    #[test]
    fn only_offline_pool_leftovers_means_no_pool_activity() {
        let runners = [
            pool_rec(10, "q7k2m9x4b1c8d3e5", false, false),
            pool_rec(11, "z8y7x6w5v4u3t2s1", false, false),
            pool_rec(12, "a1b2c3d4e5f6g7h8", false, false),
        ];
        let out = assemble(
            &org_input(RosterLevel::Full, &runners, &[], 100),
            &ObservationState::default(),
            &[],
            true,
        );
        let s = summarize(&[account(AccountStatus::Ok, out.view)]);
        assert_eq!(
            (s.pool_busy, s.own_online, s.own_offline, s.attention),
            (0, 0, 0, Severity::Ok)
        );
    }

    #[test]
    fn queued_job_for_a_pool_is_waiting_and_flagged_after_10_minutes() {
        let runners = [
            rec("o", 1, "netcup"),
            pool_rec(10, "q7k2m9x4b1c8d3e5", false, false),
        ];
        let mut job = pool_job("2/2", JobState::Queued, None);
        job.job.facts.labels = vec!["Pool-8X-Linux".into()];
        let jobs = [job];
        let input = org_input(RosterLevel::Full, &runners, &jobs, 700);
        let out = assemble(&input, &ObservationState::default(), &[], true);
        assert_eq!(out.view.waiting.len(), 1);
        let w = &out.view.waiting[0];
        assert_eq!(
            w.job.pool.as_ref().map(|p| p.stem.as_str()),
            Some("pool-8x-linux")
        );
        assert!(!w.job.hosted);
        assert!(
            w.candidates.is_empty(),
            "等臨時機器的 job 不對位自家 runner"
        );
        assert!(w.attention, "排隊 700 秒要被 flag");
        // 排隊 5 分鐘還不 flag
        let early = org_input(RosterLevel::Full, &runners, &jobs, 300);
        let out = assemble(&early, &ObservationState::default(), &[], true);
        assert!(!out.view.waiting[0].attention);
    }

    #[test]
    fn busy_only_synthesized_rows_are_classified() {
        let jobs = [
            pool_job(
                "2/2",
                JobState::InProgress,
                Some((901, "pool-8x-linux-q7k2m9x4b1c8d3e5")),
            ),
            pool_job(
                "2/3",
                JobState::InProgress,
                Some((902, "pool-8x-linux-z8y7x6w5v4u3t2s1")),
            ),
        ];
        let out = assemble(
            &org_input(RosterLevel::BusyOnly, &[], &jobs, 100),
            &ObservationState::default(),
            &[],
            true,
        );
        assert_eq!(out.view.runners.len(), 2);
        assert!(out.view.runners.iter().all(|r| r.pool.is_some()));
        assert!(out.view.jobs.iter().all(|j| j.pool.is_some() && !j.hosted));
        let s = summarize(&[account(AccountStatus::Ok, out.view)]);
        assert_eq!((s.pool_busy, s.own_online), (2, 0));
    }

    #[test]
    fn pool_jobs_get_no_duration_estimate() {
        let mut e = pool_job(
            "2/2",
            JobState::InProgress,
            Some((901, "pool-8x-linux-q7k2m9x4b1c8d3e5")),
        );
        e.typical_secs = Some(600); // 自家 runner 的歷史樣本
        let jobs = [e];
        let out = assemble(
            &org_input(RosterLevel::Full, &[], &jobs, 100),
            &ObservationState::default(),
            &[],
            true,
        );
        assert_eq!(out.view.jobs[0].typical_secs, None);
    }

    #[test]
    fn self_hosted_job_has_no_pool() {
        let jobs = [running("o/r", 5)];
        let out = assemble(
            &org_input(RosterLevel::Full, &[], &jobs, 100),
            &ObservationState::default(),
            &[],
            true,
        );
        let j = &out.view.jobs[0];
        assert!(j.pool.is_none() && !j.hosted);
    }

    #[test]
    fn a_job_view_without_a_pool_field_still_loads() {
        let old = r#"{"key":"1/2","repo":"o/r","workflow":"ci","job":"t","state":"InProgress",
            "labels":["self-hosted"],"pr_tag":"PR#1","url":"u","runner_name":"x","hosted":false,
            "typical_secs":null}"#;
        let j: JobView = serde_json::from_str(old).expect("deserialises without `pool`");
        assert!(j.pool.is_none());
    }

    fn empty_scope() -> ScopeView {
        ScopeView {
            name: "o".into(),
            kind: ScopeKind::Org,
            roster: RosterLevel::Full,
            runners: vec![],
            waiting: vec![],
            jobs: vec![],
            last_updated_at: 0,
        }
    }

    #[test]
    fn only_a_job_queued_too_long_raises_attention() {
        let mut sv = empty_scope();
        assert_eq!(
            summarize(&[account(AccountStatus::Ok, sv.clone())]).attention,
            Severity::Ok
        );
        sv.runners.push(RunnerView {
            key: "k".into(),
            name: "n".into(),
            os: String::new(),
            state: RunnerState::Offline {
                since: pr_runners_rules::OfflineSince::Unknown,
            },
            labels: vec![],
            group: None,
            job: None,
            elapsed: None,
            pool: None,
            can_take: None,
        });
        let s = summarize(&[account(AccountStatus::Ok, sv.clone())]);
        assert_eq!(
            (s.attention, s.own_offline),
            (Severity::Ok, 1),
            "離線不算需要注意"
        );
        let fleet = Fleet::default();
        sv.waiting.push(WaitingView {
            job: job_view(&running("o/r", 1), &fleet),
            age: pr_runners_rules::Elapsed::Known { secs: 700 },
            attention: true,
            candidates: vec![],
        });
        let s = summarize(&[account(AccountStatus::Ok, sv)]);
        assert_eq!((s.attention, s.waiting), (Severity::Warn, 1));
    }

    #[test]
    fn stale_when_any_account_is_offline_or_throttled() {
        let ok = account(AccountStatus::Ok, empty_scope());
        assert!(!summarize(std::slice::from_ref(&ok)).stale);
        let offline = account(AccountStatus::Offline { last_ok_at: 1 }, empty_scope());
        assert!(summarize(&[ok.clone(), offline]).stale);
        let throttled = account(
            AccountStatus::Throttled {
                remaining: 0,
                reset_at: 9,
            },
            empty_scope(),
        );
        assert!(summarize(&[ok, throttled]).stale);
    }
}
