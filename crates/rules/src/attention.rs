//! 「需要注意」規則與觀察狀態（spec §10）。純函式：
//! 輸入「上一個狀態＋這一輪看到的事實＋現在時間」，輸出新狀態、需要注意清單、狀態轉換事件。
//! runner 離線不算需要注意（自家機器離線「需要時找得到就好」、臨時機器本來就會下線）；
//! 離線時間照舊追蹤，畫面要顯示離線多久。需要注意只剩「job 排隊太久」。

use crate::model::{Elapsed, JobState};
use crate::time::queued_age;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// job 排隊超過這麼久才算需要注意。
pub const QUEUE_LIMIT_SECS: i64 = 600;
/// 距上次成功觀察超過這麼久，中間發生什麼不知道，視同第一次看到。
pub const STALE_GAP_SECS: i64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum OfflineSince {
    /// 本機親眼看到上線→離線的那一刻（Unix 秒）。
    Observed(i64),
    /// 第一次看到就已離線，或觀察中斷後降級：不知道離線多久。
    Unknown,
}

/// 要隨 snapshot 存到磁碟的觀察狀態；App 重開後延續。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ObservationState {
    /// key = runner key
    pub offline_since: BTreeMap<String, OfflineSince>,
    /// key = `JobFacts.key`；只記排隊中的 self-hosted job
    pub job_first_seen: BTreeMap<String, i64>,
    pub last_observed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunnerObs {
    pub key: String,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JobObs {
    pub key: String,
    pub state: JobState,
    pub created_at: i64,
    pub self_hosted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Attention {
    JobQueuedTooLong { key: String, age: Elapsed },
}

/// 狀態轉換事件。第一版只用 `Attention` 清單畫 UI，事件留給日後的推播。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AttentionEvent {
    JobQueuedTooLong { key: String },
    JobQueueCleared { key: String },
}

/// 呼叫端傳入的必須是該 scope 目前已知的**完整** facts：不在輸入裡的 key 會從狀態移除。
/// 沒取得資料的 scope 不要呼叫它，讓那份狀態原樣保留。
///
/// `first_observation`：App 剛啟動／Android 剛回前景後的第一輪。此時新看到的離線 runner
/// 記 `Unknown`（`prev` 已有紀錄則沿用）。距上次成功觀察超過 `STALE_GAP_SECS`
/// （或從沒觀察過）視同 `first_observation`，且 `prev` 裡的 `Observed(_)` 一律降為 `Unknown`。
pub fn evaluate(
    prev: &ObservationState,
    prev_attention: &[Attention],
    runners: &[RunnerObs],
    jobs: &[JobObs],
    now: i64,
    first_observation: bool,
) -> (ObservationState, Vec<Attention>, Vec<AttentionEvent>) {
    // 上次觀察在「未來」（系統時鐘被調回去）一樣不可信，視同觀察中斷
    let stale = prev
        .last_observed_at
        .is_none_or(|t| t > now || now - t > STALE_GAP_SECS);
    let first = first_observation || stale;

    let mut attention = Vec::new();

    // runner：只留這一輪還看得到、而且離線的（只追蹤離線多久，不產生需要注意）
    let mut offline_since = BTreeMap::new();
    for r in runners.iter().filter(|r| !r.online) {
        let since = match prev.offline_since.get(&r.key) {
            Some(OfflineSince::Observed(t)) if stale || *t > now => OfflineSince::Unknown,
            Some(known) => *known,
            None if first => OfflineSince::Unknown,
            None => OfflineSince::Observed(now),
        };
        offline_since.insert(r.key.clone(), since);
    }

    // job：只有排隊中的 self-hosted job 會被記首見時間與判斷；waiting／hosted 永遠不 flag
    let mut job_first_seen = BTreeMap::new();
    for j in jobs
        .iter()
        .filter(|j| j.state == JobState::Queued && j.self_hosted)
    {
        let first_seen = prev.job_first_seen.get(&j.key).copied().unwrap_or(now);
        job_first_seen.insert(j.key.clone(), first_seen);
        let age = queued_age(now, j.created_at);
        // created_at 不可信時，門檻改用本機首見時間
        let secs = match age {
            Elapsed::Known { secs } => secs,
            Elapsed::Untrusted => now - first_seen,
        };
        if secs > QUEUE_LIMIT_SECS {
            attention.push(Attention::JobQueuedTooLong {
                key: j.key.clone(),
                age,
            });
        }
    }

    let events = diff_events(prev_attention, &attention);
    let state = ObservationState {
        offline_since,
        job_first_seen,
        last_observed_at: Some(now),
    };
    (state, attention, events)
}

/// 事件由「上一輪」與「這一輪」需要注意的集合差產生；`age` 變了不算新事件。
fn diff_events(prev: &[Attention], next: &[Attention]) -> Vec<AttentionEvent> {
    fn key(a: &Attention) -> &str {
        match a {
            Attention::JobQueuedTooLong { key, .. } => key,
        }
    }
    let prev_keys: BTreeSet<&str> = prev.iter().map(key).collect();
    let next_keys: BTreeSet<&str> = next.iter().map(key).collect();

    let mut events = Vec::new();
    for k in next_keys.difference(&prev_keys) {
        events.push(AttentionEvent::JobQueuedTooLong { key: k.to_string() });
    }
    for k in prev_keys.difference(&next_keys) {
        events.push(AttentionEvent::JobQueueCleared { key: k.to_string() });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(key: &str, online: bool) -> RunnerObs {
        RunnerObs {
            key: key.into(),
            online,
        }
    }
    fn jo(key: &str, state: JobState, created_at: i64, self_hosted: bool) -> JobObs {
        JobObs {
            key: key.into(),
            state,
            created_at,
            self_hosted,
        }
    }
    /// 離線 runner 的追蹤結果：只看 `offline_since`，需要注意清單永遠是空的。
    fn run(
        prev: &ObservationState,
        runners: &[RunnerObs],
        now: i64,
        first: bool,
    ) -> ObservationState {
        let (s, a, ev) = evaluate(prev, &[], runners, &[], now, first);
        assert!(a.is_empty(), "runner 離線不算需要注意");
        assert!(ev.is_empty(), "runner 離線不產生事件");
        s
    }

    #[test]
    fn offline_runner_is_never_flagged_but_still_tracked() {
        let s1 = run(
            &ObservationState::default(),
            &[ro("r1", true)],
            1_000,
            false,
        );
        let s2 = run(&s1, &[ro("r1", false)], 1_100, false);
        assert_eq!(s2.offline_since["r1"], OfflineSince::Observed(1_100));
        // 離線很久也不變成需要注意，離線起點照舊
        let s3 = run(&s2, &[ro("r1", false)], 1_500, false);
        assert_eq!(s3.offline_since["r1"], OfflineSince::Observed(1_100));
    }

    #[test]
    fn offline_on_first_observation_is_unknown() {
        let s = run(
            &ObservationState::default(),
            &[ro("r1", false)],
            1_000,
            true,
        );
        assert_eq!(s.offline_since["r1"], OfflineSince::Unknown);
    }

    #[test]
    fn persisted_observed_since_survives_restart() {
        // prev 已有 Observed(500)、last_observed_at=Some(600)；在 1_000 重開（間隔 400 ≤ 900），
        // first_observation=true 仍沿用 500
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(600);
        let s = run(&prev, &[ro("r1", false)], 1_000, true);
        assert_eq!(s.offline_since["r1"], OfflineSince::Observed(500));
    }

    #[test]
    fn long_gap_downgrades_observed_to_unknown() {
        // Review Focus 7：Mac 睡了一晚，不能顯示「離線 3 分鐘」
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(600);
        let s = run(&prev, &[ro("r1", false)], 30_000, false);
        assert_eq!(s.offline_since["r1"], OfflineSince::Unknown);
    }

    #[test]
    fn gap_of_exactly_stale_limit_is_not_stale() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(1_000);
        let s = run(&prev, &[ro("r1", false)], 1_900, false);
        assert_eq!(s.offline_since["r1"], OfflineSince::Observed(500));
    }

    #[test]
    fn clock_rollback_never_leaves_a_future_offline_start() {
        // 系統時鐘被調回去：持久化的 Observed(t) 在「未來」，不能顯示成負的離線時間
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(5_000));
        prev.last_observed_at = Some(5_100);
        let s = run(&prev, &[ro("r1", false)], 1_000, false);
        assert_eq!(s.offline_since["r1"], OfflineSince::Unknown);
        assert_eq!(s.last_observed_at, Some(1_000));
    }

    #[test]
    fn last_observation_in_the_future_counts_as_an_observation_gap() {
        // 時鐘被調回去：上次觀察時間在「未來」，這一輪新看到的離線不能當成剛剛才離線
        let prev = ObservationState {
            last_observed_at: Some(5_000),
            ..Default::default()
        };
        let s = run(&prev, &[ro("r1", false)], 1_000, false);
        assert_eq!(s.offline_since["r1"], OfflineSince::Unknown);
    }

    #[test]
    fn observed_start_in_the_future_alone_is_downgraded() {
        // last_observed_at 看起來正常，但某筆 Observed(t) 的 t 在未來
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(2_000));
        prev.last_observed_at = Some(900);
        let s = run(&prev, &[ro("r1", false)], 1_000, false);
        assert_eq!(s.offline_since["r1"], OfflineSince::Unknown);
    }

    #[test]
    fn recovery_forgets_the_offline_start() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(900);
        let s = run(&prev, &[ro("r1", true)], 1_000, false);
        assert!(s.offline_since.is_empty());
    }

    #[test]
    fn removed_runner_is_dropped() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Unknown);
        prev.last_observed_at = Some(900);
        let s = run(&prev, &[], 1_000, false);
        assert!(s.offline_since.is_empty());
    }

    #[test]
    fn queued_over_limit_flags() {
        let j = jo("10/1", JobState::Queued, 300, true);
        let (_, a, ev) = evaluate(&ObservationState::default(), &[], &[], &[j], 901, true);
        assert_eq!(
            a,
            vec![Attention::JobQueuedTooLong {
                key: "10/1".into(),
                age: Elapsed::Known { secs: 601 }
            }]
        );
        assert_eq!(
            ev,
            vec![AttentionEvent::JobQueuedTooLong { key: "10/1".into() }]
        );
    }

    #[test]
    fn queued_exactly_at_limit_does_not_flag() {
        let j = jo("10/1", JobState::Queued, 300, true);
        let (_, a, _) = evaluate(&ObservationState::default(), &[], &[], &[j], 900, true);
        assert!(a.is_empty());
    }

    #[test]
    fn queued_with_future_created_uses_first_seen() {
        // created_at=99_999（未來）；first_seen 在 1_000 記錄；到 1_601 才 flag，age = Untrusted
        let j = jo("10/1", JobState::Queued, 99_999, true);
        let (s1, a1, _) = evaluate(
            &ObservationState::default(),
            &[],
            &[],
            std::slice::from_ref(&j),
            1_000,
            true,
        );
        assert!(a1.is_empty());
        assert_eq!(s1.job_first_seen["10/1"], 1_000);
        let (s2, a2, _) = evaluate(&s1, &a1, &[], std::slice::from_ref(&j), 1_600, false);
        assert!(a2.is_empty()); // 600 秒，恰好門檻
        let (_, a3, _) = evaluate(&s2, &a2, &[], &[j], 1_601, false);
        assert_eq!(
            a3,
            vec![Attention::JobQueuedTooLong {
                key: "10/1".into(),
                age: Elapsed::Untrusted
            }]
        );
    }

    #[test]
    fn waiting_job_never_flags() {
        // Review Focus 4
        let j = jo("10/2", JobState::Waiting, 0, true);
        let (s, a, _) = evaluate(&ObservationState::default(), &[], &[], &[j], 10_000, true);
        assert!(a.is_empty());
        assert!(s.job_first_seen.is_empty());
    }

    #[test]
    fn hosted_queued_job_never_flags() {
        let j = jo("10/3", JobState::Queued, 0, false);
        let (_, a, _) = evaluate(&ObservationState::default(), &[], &[], &[j], 10_000, true);
        assert!(a.is_empty());
    }

    #[test]
    fn job_leaving_queue_clears_and_forgets_first_seen() {
        let j = jo("10/1", JobState::Queued, 0, true);
        let (s1, a1, _) = evaluate(&ObservationState::default(), &[], &[], &[j], 1_000, true);
        assert_eq!(a1.len(), 1);
        assert!(s1.job_first_seen.contains_key("10/1"));
        let (s2, a2, ev) = evaluate(&s1, &a1, &[], &[], 1_030, false);
        assert!(a2.is_empty());
        assert!(s2.job_first_seen.is_empty());
        assert_eq!(
            ev,
            vec![AttentionEvent::JobQueueCleared { key: "10/1".into() }]
        );
    }

    #[test]
    fn job_started_running_clears_queue_attention() {
        let queued = jo("10/1", JobState::Queued, 0, true);
        let (s1, a1, _) = evaluate(
            &ObservationState::default(),
            &[],
            &[],
            &[queued],
            1_000,
            true,
        );
        let running = jo("10/1", JobState::InProgress, 0, true);
        let (s2, a2, ev) = evaluate(&s1, &a1, &[], &[running], 1_030, false);
        assert!(a2.is_empty() && s2.job_first_seen.is_empty());
        assert_eq!(
            ev,
            vec![AttentionEvent::JobQueueCleared { key: "10/1".into() }]
        );
    }

    #[test]
    fn state_roundtrips_through_json() {
        let mut s = ObservationState::default();
        s.offline_since
            .insert("a".into(), OfflineSince::Observed(5));
        s.offline_since.insert("b".into(), OfflineSince::Unknown);
        s.job_first_seen.insert("1/2".into(), 7);
        s.last_observed_at = Some(9);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<ObservationState>(&json).unwrap(), s);
    }
}
