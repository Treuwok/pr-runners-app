//! runner 執行歷史：每台 runner 跑過哪些 job、結果如何。
//!
//! 兩個來源：
//! - **記錄**：每一輪把「正在某台 runner 上跑」的 job 記下來；job 從畫面上消失後，
//!   查一次它的最終結果（每個 job 一次請求，每個帳號每輪有上限）。
//! - **補查**：使用者按「載入更早的紀錄」時，查範圍內最近有推送的 repo 的已完成執行，
//!   挑出跑在這台 runner 上的 job（只在按下時才花請求）。
//!
//! 每台 runner 只留最近 `MAX_PER_RUNNER` 筆、最多 `KEEP_SECS` 天。

use crate::api::{JobSnapshot, RunInfo};
use crate::view::{AccountView, RunnerState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_PER_RUNNER: usize = 200;
pub const KEEP_SECS: i64 = 30 * 24 * 3_600;
/// 還在跑的 job 查過一次後，隔這麼久才再查（避免每輪重查同一個）。
const RECHECK_SECS: i64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub enum JobOutcome {
    Running,
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
    /// 其他結論（neutral、action_required…），或 GitHub 已查不到這個 job
    Other,
}

impl JobOutcome {
    /// GitHub 的 status／conclusion → 結果。還沒完成的一律算 Running。
    pub fn from_github(status: &str, conclusion: Option<&str>) -> Self {
        if status != "completed" {
            return Self::Running;
        }
        match conclusion {
            Some("success") => Self::Success,
            Some("failure") => Self::Failure,
            Some("cancelled") => Self::Cancelled,
            Some("skipped") => Self::Skipped,
            Some("timed_out") => Self::TimedOut,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub enum RecordSource {
    /// App 開著時親眼看到它在跑
    Observed,
    /// 使用者按「載入更早的紀錄」時從 GitHub 補查到的
    Backfill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct JobRecord {
    pub job_id: u64,
    pub run_id: u64,
    pub repo: String,
    pub workflow: String,
    pub job: String,
    pub pr_tag: String,
    pub url: String,
    /// Unix 秒；GitHub 的時間不可信時是 null
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub outcome: JobOutcome,
    pub source: RecordSource,
    /// 上次去 GitHub 查結果的時間（只對還在跑的有意義）
    #[ts(skip)]
    #[serde(default)]
    pub checked_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerLog {
    pub account_id: u64,
    /// 新到舊
    pub records: Vec<JobRecord>,
}

/// 補查的結果（給畫面顯示「查了幾個 repo、補到幾筆」）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct BackfillReport {
    pub repos_checked: u32,
    pub runs_checked: u32,
    pub added: u32,
    /// 有 repo 查不到（權限或網路）
    pub partial: bool,
}

/// key＝runner 的 key（與 `RunnerView.key` 相同）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLog {
    pub runners: BTreeMap<String, RunnerLog>,
}

fn sort_key(r: &JobRecord) -> i64 {
    r.started_at.or(r.completed_at).unwrap_or(0)
}

impl RunLog {
    /// 某台 runner 的紀錄（新到舊）。
    pub fn records(&self, runner_key: &str) -> Vec<JobRecord> {
        self.runners
            .get(runner_key)
            .map(|l| l.records.clone())
            .unwrap_or_default()
    }

    /// 記下這一輪畫面上正在跑的 job；已記過的不重複。
    pub fn observe(&mut self, views: &[AccountView], now: i64) {
        for a in views {
            for s in &a.scopes {
                for r in &s.runners {
                    let (RunnerState::Busy, Some(job)) = (&r.state, &r.job) else {
                        continue;
                    };
                    let Some((run_id, job_id)) = parse_job_key(&job.key) else {
                        continue;
                    };
                    let started_at = match r.elapsed {
                        Some(pr_runners_rules::Elapsed::Known { secs }) => Some(now - secs),
                        _ => None,
                    };
                    self.insert(
                        &r.key,
                        a.account_id,
                        JobRecord {
                            job_id,
                            run_id,
                            repo: job.repo.clone(),
                            workflow: job.workflow.clone(),
                            job: job.job.clone(),
                            pr_tag: job.pr_tag.clone(),
                            url: job.url.clone(),
                            started_at,
                            completed_at: None,
                            outcome: JobOutcome::Running,
                            source: RecordSource::Observed,
                            checked_at: now,
                        },
                    );
                }
            }
        }
    }

    /// 加入一筆；同一個 job 已經有了就不動（第一次看到的資訊為準）。回傳是否真的加入。
    pub fn insert(&mut self, runner_key: &str, account_id: u64, rec: JobRecord) -> bool {
        let log = self
            .runners
            .entry(runner_key.to_string())
            .or_insert_with(|| RunnerLog {
                account_id,
                records: Vec::new(),
            });
        if log.records.iter().any(|r| r.job_id == rec.job_id) {
            return false;
        }
        log.records.push(rec);
        log.records.sort_by_key(|r| std::cmp::Reverse(sort_key(r)));
        true
    }

    /// 該去查最終結果的 job：還標著 Running、已經不在**同一個帳號**的畫面上、而且距上次查超過 `RECHECK_SECS`。
    /// `active` 是 `(account_id, job_id)`：別的帳號暫停時殘留的舊畫面，不能擋住這個帳號查結果。
    /// 回傳 `(runner key, account id, repo, job id)`，每個帳號最多 `per_account` 個。
    pub fn due_for_result(
        &self,
        active: &BTreeSet<(u64, u64)>,
        now: i64,
        per_account: usize,
    ) -> Vec<(String, u64, String, u64)> {
        // 先依「上次查詢時間」由舊到新排，再套用每個帳號的上限：
        // 一直查不到結果的 job 不能每輪都佔滿額度，讓後面已經結束的 job 永遠輪不到
        let mut eligible: Vec<(i64, &String, u64, &JobRecord)> = self
            .runners
            .iter()
            .flat_map(|(key, log)| {
                log.records
                    .iter()
                    .map(move |r| (r.checked_at, key, log.account_id, r))
            })
            .filter(|(_, _, account_id, r)| {
                r.outcome == JobOutcome::Running
                    && !active.contains(&(*account_id, r.job_id))
                    && now - r.checked_at >= RECHECK_SECS
            })
            .collect();
        eligible.sort_by_key(|(checked_at, ..)| *checked_at);
        let mut taken: BTreeMap<u64, usize> = BTreeMap::new();
        let mut out = Vec::new();
        for (_, key, account_id, r) in eligible {
            let n = taken.entry(account_id).or_default();
            if *n >= per_account {
                continue;
            }
            *n += 1;
            out.push((key.clone(), account_id, r.repo.clone(), r.job_id));
        }
        out
    }

    /// 套用查到的結果。`None`＝GitHub 查不到這個 job 了：標成 Other，不再重查。
    pub fn apply_result(
        &mut self,
        runner_key: &str,
        job_id: u64,
        got: Option<&JobSnapshot>,
        now: i64,
    ) {
        let Some(log) = self.runners.get_mut(runner_key) else {
            return;
        };
        let Some(r) = log.records.iter_mut().find(|r| r.job_id == job_id) else {
            return;
        };
        r.checked_at = now;
        match got {
            None => r.outcome = JobOutcome::Other,
            Some(j) => {
                r.outcome = JobOutcome::from_github(&j.status, j.conclusion.as_deref());
                if r.outcome != JobOutcome::Running {
                    r.completed_at = j.completed_at;
                    r.started_at = r.started_at.or(j.started_at);
                }
            }
        }
    }

    /// 查詢暫時失敗：記下查過的時間，隔 `RECHECK_SECS` 再試，結果不動。
    pub fn touch(&mut self, runner_key: &str, job_id: u64, now: i64) {
        if let Some(r) = self
            .runners
            .get_mut(runner_key)
            .and_then(|l| l.records.iter_mut().find(|r| r.job_id == job_id))
        {
            r.checked_at = now;
        }
    }

    /// 只留每台最近 `MAX_PER_RUNNER` 筆、`KEEP_SECS` 以內的；被移除的帳號整個丟掉。
    pub fn prune(&mut self, now: i64, accounts: &BTreeSet<u64>) {
        self.runners.retain(|_, l| accounts.contains(&l.account_id));
        for log in self.runners.values_mut() {
            log.records
                .retain(|r| r.outcome == JobOutcome::Running || now - sort_key(r) < KEEP_SECS);
            log.records.truncate(MAX_PER_RUNNER);
        }
        self.runners.retain(|_, l| !l.records.is_empty());
    }
}

/// `JobView.key` 是 `"{run_id}/{job_id}"`。
fn parse_job_key(key: &str) -> Option<(u64, u64)> {
    let (run, job) = key.split_once('/')?;
    Some((run.parse().ok()?, job.parse().ok()?))
}

/// 補查：一次執行裡跑在這台 runner 上的 job → 紀錄。
/// 有 runner id 就只認 id（同一個帳號下可能有別台同名的 runner）；沒有 id 才退回比名稱。
pub fn backfill_records(
    run: &RunInfo,
    jobs: &[JobSnapshot],
    runner_id: Option<u64>,
    runner_name: &str,
    tag: &str,
) -> Vec<JobRecord> {
    jobs.iter()
        .filter(|j| match runner_id {
            Some(id) => j.runner_id == Some(id),
            None => j.runner_name.as_deref() == Some(runner_name),
        })
        .map(|j| JobRecord {
            job_id: j.job_id,
            run_id: run.id,
            repo: run.repo.clone(),
            workflow: j.workflow_name.clone(),
            job: j.name.clone(),
            pr_tag: tag.to_string(),
            url: j.html_url.clone(),
            started_at: j.started_at,
            completed_at: j.completed_at,
            outcome: JobOutcome::from_github(&j.status, j.conclusion.as_deref()),
            source: RecordSource::Backfill,
            checked_at: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(job_id: u64, started: i64, outcome: JobOutcome) -> JobRecord {
        JobRecord {
            job_id,
            run_id: 1,
            repo: "o/r".into(),
            workflow: "ci".into(),
            job: "build".into(),
            pr_tag: "PR#1".into(),
            url: String::new(),
            started_at: Some(started),
            completed_at: None,
            outcome,
            source: RecordSource::Observed,
            checked_at: 0,
        }
    }

    #[test]
    fn outcome_from_github() {
        assert_eq!(
            JobOutcome::from_github("in_progress", None),
            JobOutcome::Running
        );
        assert_eq!(
            JobOutcome::from_github("completed", Some("success")),
            JobOutcome::Success
        );
        assert_eq!(
            JobOutcome::from_github("completed", Some("failure")),
            JobOutcome::Failure
        );
        assert_eq!(
            JobOutcome::from_github("completed", Some("cancelled")),
            JobOutcome::Cancelled
        );
        assert_eq!(
            JobOutcome::from_github("completed", Some("timed_out")),
            JobOutcome::TimedOut
        );
        assert_eq!(
            JobOutcome::from_github("completed", Some("neutral")),
            JobOutcome::Other
        );
    }

    #[test]
    fn insert_dedups_and_keeps_newest_first() {
        let mut log = RunLog::default();
        assert!(log.insert("k", 1, rec(1, 100, JobOutcome::Running)));
        assert!(log.insert("k", 1, rec(2, 300, JobOutcome::Success)));
        assert!(log.insert("k", 1, rec(3, 200, JobOutcome::Failure)));
        assert!(
            !log.insert("k", 1, rec(2, 999, JobOutcome::Failure)),
            "同一個 job 不重複"
        );
        let ids: Vec<u64> = log.records("k").iter().map(|r| r.job_id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
        assert_eq!(
            log.records("k")[0].outcome,
            JobOutcome::Success,
            "第一次的資訊為準"
        );
    }

    #[test]
    fn only_running_jobs_that_left_the_screen_are_checked_with_a_per_account_cap() {
        let mut log = RunLog::default();
        for id in 1..=5 {
            log.insert("a", 1, rec(id, 100, JobOutcome::Running));
        }
        log.insert("a", 1, rec(9, 100, JobOutcome::Success));
        log.insert("b", 2, rec(20, 100, JobOutcome::Running));
        let active = BTreeSet::from([(1, 1)]);
        let due = log.due_for_result(&active, 1_000, 3);
        let ids: Vec<(u64, u64)> = due.iter().map(|d| (d.1, d.3)).collect();
        assert_eq!(
            ids.iter().filter(|(a, _)| *a == 1).count(),
            3,
            "帳號 1 最多 3 個"
        );
        assert!(!ids.contains(&(1, 1)), "還在畫面上的不查");
        assert!(!ids.contains(&(1, 9)), "已有結果的不查");
        assert!(ids.contains(&(2, 20)), "帳號 2 有自己的額度");
        // 剛查過的不重查
        assert!(log.due_for_result(&active, 50, 3).is_empty());
    }

    #[test]
    fn least_recently_checked_jobs_go_first_so_retries_cannot_starve_others() {
        // runner "a" 有 3 個一直查不到結果的 job（剛好過了重查間隔）；runner "z" 有 1 個從沒查過
        let mut log = RunLog::default();
        for id in 1..=3 {
            let mut r = rec(id, 100, JobOutcome::Running);
            r.checked_at = 880;
            log.insert("a", 1, r);
        }
        log.insert("z", 1, rec(9, 100, JobOutcome::Running));
        let due = log.due_for_result(&BTreeSet::new(), 1_000, 2);
        let ids: Vec<u64> = due.iter().map(|d| d.3).collect();
        assert_eq!(ids[0], 9, "從沒查過的先查，不會被前面一直重試的擋住");
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn a_stale_view_in_one_account_does_not_block_another_accounts_lookup() {
        // 帳號 2 暫停（畫面殘留舊的「還在跑」）；帳號 1 已經看到它結束了
        let mut log = RunLog::default();
        log.insert("a1", 1, rec(5, 100, JobOutcome::Running));
        let active = BTreeSet::from([(2, 5)]);
        let due = log.due_for_result(&active, 1_000, 3);
        assert_eq!(due.len(), 1, "帳號 1 的紀錄要去查結果");
        assert_eq!((due[0].1, due[0].3), (1, 5));
    }

    #[test]
    fn applying_results() {
        let mut log = RunLog::default();
        log.insert("k", 1, rec(1, 100, JobOutcome::Running));
        log.insert("k", 1, rec(2, 90, JobOutcome::Running));
        let done = JobSnapshot {
            job_id: 1,
            run_id: 1,
            name: "build".into(),
            workflow_name: "ci".into(),
            status: "completed".into(),
            conclusion: Some("failure".into()),
            started_at: Some(100),
            completed_at: Some(700),
            runner_id: Some(9),
            runner_name: Some("x".into()),
            html_url: String::new(),
        };
        log.apply_result("k", 1, Some(&done), 800);
        log.apply_result("k", 2, None, 800);
        let r = log.records("k");
        assert_eq!(
            (r[0].outcome, r[0].completed_at),
            (JobOutcome::Failure, Some(700))
        );
        assert_eq!(r[1].outcome, JobOutcome::Other, "查不到的不再重查");
    }

    #[test]
    fn prune_keeps_recent_and_caps_per_runner() {
        let mut log = RunLog::default();
        let now = 100 * 24 * 3_600;
        log.insert("old", 1, rec(1, now - KEEP_SECS - 10, JobOutcome::Success));
        log.insert("old", 1, rec(2, now - KEEP_SECS - 10, JobOutcome::Running));
        for id in 0..(MAX_PER_RUNNER as u64 + 5) {
            log.insert(
                "busy",
                1,
                rec(100 + id, now - id as i64, JobOutcome::Success),
            );
        }
        log.insert("gone", 2, rec(7, now, JobOutcome::Success));
        log.prune(now, &BTreeSet::from([1]));
        assert_eq!(log.records("old").len(), 1, "太舊的刪掉，還在跑的留著");
        assert_eq!(log.records("busy").len(), MAX_PER_RUNNER);
        assert_eq!(log.records("busy")[0].job_id, 100, "留最新的");
        assert!(log.records("gone").is_empty(), "移除的帳號整個丟掉");
    }

    #[test]
    fn parses_job_keys() {
        assert_eq!(parse_job_key("123/456"), Some((123, 456)));
        assert_eq!(parse_job_key("bad"), None);
    }
}
