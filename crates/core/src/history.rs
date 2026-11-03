//! 歷次執行時間：預估正在跑的 job 大概還要多久。
//!
//! - 只查「現在有 job 在跑」的 workflow，最近 `RUNS_PER_WORKFLOW` 次成功的執行。
//! - 已完成的執行不會再變：每次執行的 job 清單只抓一次，之後一直用快取。
//! - workflow 的執行清單每 `REFRESH_SECS` 才重新確認一次（有 ETag，沒變就不計費）。
//! - 每個帳號每一輪最多補抓 `MAX_NEW_RUNS_PER_TICK` 次執行（所有範圍共用），分攤到幾輪，不讓單一輪暴衝。
//! - 樣本不足就不預估（寧可說不知道）。

use crate::api;
use crate::client::{ApiError, GitHubClient};
use futures_util::future::join_all;
use std::collections::{BTreeMap, BTreeSet};

/// 至少要有這麼多次成功紀錄才預估。
pub const MIN_SAMPLES: usize = 3;
const RUNS_PER_WORKFLOW: u32 = 20;
const REFRESH_SECS: i64 = 900;
const MAX_NEW_RUNS_PER_TICK: usize = 10;
/// 一天沒再用到的 workflow 就丟掉（連同它的執行紀錄）。
const FORGET_AFTER_SECS: i64 = 24 * 3_600;

/// 一個 job 在某次成功執行裡跑了多久。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub job_name: String,
    pub secs: u64,
    pub runner: Option<String>,
}

/// 中位數（偶數個取中間兩個的平均）：一次異常慢的執行不會把預估拉歪。
fn median(mut v: Vec<u64>) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let mid = v.len() / 2;
    Some(if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2
    } else {
        v[mid]
    })
}

/// 同名 job 平常要跑多久。同一台 runner 有足夠紀錄就只看它（機器快慢差很多，
/// 實測同一個 Unit Tests 在兩台機器上是 7 分與 17 分）；不夠就看所有 runner；再不夠就不預估。
pub fn typical_secs<'a>(
    samples: impl IntoIterator<Item = &'a Sample>,
    job_name: &str,
    runner: Option<&str>,
) -> Option<u64> {
    let same_job: Vec<&Sample> = samples
        .into_iter()
        .filter(|s| s.job_name == job_name)
        .collect();
    if let Some(r) = runner {
        let mine: Vec<u64> = same_job
            .iter()
            .filter(|s| s.runner.as_deref() == Some(r))
            .map(|s| s.secs)
            .collect();
        if mine.len() >= MIN_SAMPLES {
            return median(mine);
        }
    }
    if same_job.len() >= MIN_SAMPLES {
        return median(same_job.iter().map(|s| s.secs).collect());
    }
    None
}

/// `(repo 全名, workflow id)`
pub type WorkflowKey = (String, u64);

struct WorkflowRuns {
    checked_at: i64,
    used_at: i64,
    /// 最近成功的執行，新到舊
    run_ids: Vec<u64>,
}

/// 一個帳號的歷次執行快取（不同帳號的權限不同，不共用）。
#[derive(Default)]
pub struct JobHistory {
    workflows: BTreeMap<WorkflowKey, WorkflowRuns>,
    /// `(repo, run id)` → 那次執行裡成功 job 的時長；已完成的執行不會再變
    runs: BTreeMap<(String, u64), Vec<Sample>>,
    /// 這一輪還能補抓幾次執行；帳號的所有範圍共用，`start_tick` 重設
    budget: usize,
}

impl JobHistory {
    /// 每一輪開始時呼叫一次（在掃任何範圍之前），重設這一輪的補抓額度。
    pub fn start_tick(&mut self) {
        self.budget = MAX_NEW_RUNS_PER_TICK;
    }

    /// 為這一輪正在跑的 workflow 補齊歷史。查詢失敗不影響畫面：那個 workflow 這輪就沒有預估。
    pub async fn refresh(&mut self, c: &GitHubClient, wanted: &BTreeSet<WorkflowKey>, now: i64) {
        // 1) 執行清單：沒查過或過期才查
        for key in wanted {
            let stale = self
                .workflows
                .get(key)
                .is_none_or(|w| now - w.checked_at >= REFRESH_SECS);
            if stale
                && let Ok(ids) = api::successful_run_ids(c, &key.0, key.1, RUNS_PER_WORKFLOW).await
            {
                self.workflows.insert(
                    key.clone(),
                    WorkflowRuns {
                        checked_at: now,
                        used_at: now,
                        run_ids: ids,
                    },
                );
            }
            if let Some(w) = self.workflows.get_mut(key) {
                w.used_at = now;
            }
        }
        // 2) 還沒抓過的執行：用這一輪剩下的額度（所有範圍共用），平行抓
        let missing: Vec<(String, u64)> = wanted
            .iter()
            .filter_map(|k| self.workflows.get(k).map(|w| (k, w)))
            .flat_map(|(k, w)| w.run_ids.iter().map(move |id| (k.0.clone(), *id)))
            .filter(|run| !self.runs.contains_key(run))
            .take(self.budget)
            .collect();
        self.budget -= missing.len();
        let fetched = join_all(
            missing
                .iter()
                .map(|(repo, id)| api::completed_job_durations(c, repo, *id)),
        )
        .await;
        for (run, res) in missing.into_iter().zip(fetched) {
            let samples: Result<Vec<Sample>, ApiError> = res;
            if let Ok(samples) = samples {
                self.runs.insert(run, samples);
            }
        }
        self.forget_unused(now);
    }

    /// 一天沒用到的 workflow 與不再被任何 workflow 參照的執行，從記憶體移除（長時間執行不無限長大）。
    fn forget_unused(&mut self, now: i64) {
        self.workflows
            .retain(|_, w| now - w.used_at < FORGET_AFTER_SECS);
        let referenced: BTreeSet<(String, u64)> = self
            .workflows
            .iter()
            .flat_map(|(k, w)| w.run_ids.iter().map(move |id| (k.0.clone(), *id)))
            .collect();
        self.runs.retain(|k, _| referenced.contains(k));
    }

    /// 這個 job 平常要跑多久（秒）；沒有足夠紀錄就是 `None`。
    pub fn typical(&self, key: &WorkflowKey, job_name: &str, runner: Option<&str>) -> Option<u64> {
        let w = self.workflows.get(key)?;
        let samples = w
            .run_ids
            .iter()
            .filter_map(|id| self.runs.get(&(key.0.clone(), *id)))
            .flatten();
        typical_secs(samples, job_name, runner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(job: &str, secs: u64, runner: &str) -> Sample {
        Sample {
            job_name: job.into(),
            secs,
            runner: Some(runner.into()),
        }
    }

    #[test]
    fn median_of_odd_and_even_counts() {
        assert_eq!(median(vec![30, 10, 20]), Some(20));
        assert_eq!(median(vec![40, 10, 20, 30]), Some(25));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn prefers_the_same_runner_when_it_has_enough_samples() {
        let v = [
            s("Unit", 420, "mac1"),
            s("Unit", 430, "mac1"),
            s("Unit", 440, "mac1"),
            s("Unit", 1_000, "box1"),
            s("Unit", 1_020, "box1"),
        ];
        assert_eq!(typical_secs(&v, "Unit", Some("mac1")), Some(430));
        // box1 只有 2 筆：改看所有 runner
        assert_eq!(typical_secs(&v, "Unit", Some("box1")), Some(440));
        assert_eq!(typical_secs(&v, "Unit", None), Some(440));
    }

    #[test]
    fn too_few_samples_means_no_estimate() {
        let v = [s("Unit", 420, "a"), s("Unit", 430, "b"), s("Other", 9, "a")];
        assert_eq!(typical_secs(&v, "Unit", Some("a")), None, "只有 2 筆 Unit");
        assert_eq!(typical_secs(&v, "Missing", None), None);
    }

    #[test]
    fn job_names_must_match_exactly() {
        let v = [
            s("E2E (shard 1/2)", 1_700, "a"),
            s("E2E (shard 1/2)", 1_800, "a"),
            s("E2E (shard 1/2)", 1_900, "a"),
            s("E2E (shard 2/2)", 900, "a"),
        ];
        assert_eq!(
            typical_secs(&v, "E2E (shard 1/2)", Some("a")),
            Some(1_800)
        );
        assert_eq!(typical_secs(&v, "E2E (shard 2/2)", Some("a")), None);
    }
}
