//! 「需要注意」規則與觀察狀態（spec §10）。純函式：
//! 輸入「上一個狀態＋這一輪看到的事實＋現在時間」，輸出新狀態、需要注意清單、狀態轉換事件。

use crate::model::{Elapsed, JobState};
use crate::time::queued_age;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 觀察到離線之後，連續離線超過這麼久才算需要注意（避開 runner 自動更新的短暫斷線）。
pub const OFFLINE_GRACE_SECS: i64 = 180;
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
    RunnerOffline { key: String, since: OfflineSince },
    JobQueuedTooLong { key: String, age: Elapsed },
}

/// 狀態轉換事件。第一版只用 `Attention` 清單畫 UI，事件留給日後的推播。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AttentionEvent {
    RunnerWentOffline { key: String },
    RunnerRecovered { key: String },
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

    // runner：只留這一輪還看得到、而且離線的
    let mut offline_since = BTreeMap::new();
    for r in runners.iter().filter(|r| !r.online) {
        let since = match prev.offline_since.get(&r.key) {
            Some(OfflineSince::Observed(t)) if stale || *t > now => OfflineSince::Unknown,
            Some(known) => *known,
            None if first => OfflineSince::Unknown,
            None => OfflineSince::Observed(now),
        };
        let flagged = match since {
            OfflineSince::Unknown => true,
            OfflineSince::Observed(t) => now - t > OFFLINE_GRACE_SECS,
        };
        if flagged {
            attention.push(Attention::RunnerOffline {
                key: r.key.clone(),
                since,
            });
        }
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

    let events = diff_events(prev_attention, &attention, runners);
    let state = ObservationState {
        offline_since,
        job_first_seen,
        last_observed_at: Some(now),
    };
    (state, attention, events)
}

/// 事件由「上一輪」與「這一輪」需要注意的集合差產生；`since`／`age` 變了不算新事件。
fn diff_events(
    prev: &[Attention],
    next: &[Attention],
    runners: &[RunnerObs],
) -> Vec<AttentionEvent> {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Id<'a> {
        Runner(&'a str),
        Job(&'a str),
    }
    fn id(a: &Attention) -> Id<'_> {
        match a {
            Attention::RunnerOffline { key, .. } => Id::Runner(key),
            Attention::JobQueuedTooLong { key, .. } => Id::Job(key),
        }
    }
    let prev_ids: BTreeSet<Id> = prev.iter().map(id).collect();
    let next_ids: BTreeSet<Id> = next.iter().map(id).collect();

    let mut events = Vec::new();
    for a in next {
        if !prev_ids.contains(&id(a)) {
            events.push(match a {
                Attention::RunnerOffline { key, .. } => {
                    AttentionEvent::RunnerWentOffline { key: key.clone() }
                }
                Attention::JobQueuedTooLong { key, .. } => {
                    AttentionEvent::JobQueuedTooLong { key: key.clone() }
                }
            });
        }
    }
    for a in prev {
        if next_ids.contains(&id(a)) {
            continue;
        }
        match a {
            // runner 被刪掉（輸入裡不見了）不是「恢復上線」，不發事件
            Attention::RunnerOffline { key, .. } => {
                if runners.iter().any(|r| r.online && &r.key == key) {
                    events.push(AttentionEvent::RunnerRecovered { key: key.clone() });
                }
            }
            Attention::JobQueuedTooLong { key, .. } => {
                events.push(AttentionEvent::JobQueueCleared { key: key.clone() });
            }
        }
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
    fn offline(key: &str, since: OfflineSince) -> Attention {
        Attention::RunnerOffline {
            key: key.into(),
            since,
        }
    }

    #[test]
    fn offline_seen_transition_waits_grace() {
        let s0 = ObservationState::default();
        // s0 沒有 last_observed_at → 視同首次，r1 在線不受影響
        let (s1, a1, _) = evaluate(&s0, &[], &[ro("r1", true)], &[], 1_000, false);
        let (s2, a2, _) = evaluate(&s1, &a1, &[ro("r1", false)], &[], 1_100, false);
        assert!(a2.is_empty()); // 剛離線，0 秒 < 180
        let (_, a3, ev) = evaluate(&s2, &a2, &[ro("r1", false)], &[], 1_281, false);
        assert_eq!(a3, vec![offline("r1", OfflineSince::Observed(1_100))]);
        assert_eq!(
            ev,
            vec![AttentionEvent::RunnerWentOffline { key: "r1".into() }]
        );
    }

    #[test]
    fn offline_exactly_at_grace_does_not_flag_yet() {
        let s0 = ObservationState::default();
        let (s1, a1, _) = evaluate(&s0, &[], &[ro("r1", true)], &[], 1_000, false);
        let (s2, a2, _) = evaluate(&s1, &a1, &[ro("r1", false)], &[], 1_100, false);
        let (_, a3, _) = evaluate(&s2, &a2, &[ro("r1", false)], &[], 1_280, false);
        assert!(a3.is_empty()); // 恰好 180 秒：「超過」才算
    }

    #[test]
    fn offline_on_first_observation_is_immediate_unknown() {
        let (_, a, ev) = evaluate(
            &ObservationState::default(),
            &[],
            &[ro("r1", false)],
            &[],
            1_000,
            true,
        );
        assert_eq!(a, vec![offline("r1", OfflineSince::Unknown)]);
        assert_eq!(
            ev,
            vec![AttentionEvent::RunnerWentOffline { key: "r1".into() }]
        );
    }

    #[test]
    fn persisted_observed_since_survives_restart() {
        // prev 已有 Observed(500)、last_observed_at=Some(600)；在 1_000 重開（間隔 400 ≤ 900），
        // first_observation=true 仍沿用 500
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(600);
        let (s, a, _) = evaluate(&prev, &[], &[ro("r1", false)], &[], 1_000, true);
        assert_eq!(a, vec![offline("r1", OfflineSince::Observed(500))]);
        assert_eq!(s.offline_since["r1"], OfflineSince::Observed(500));
    }

    #[test]
    fn long_gap_downgrades_observed_to_unknown() {
        // Review Focus 7：Mac 睡了一晚，不能顯示「離線 3 分鐘」
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(600);
        let (s, a, _) = evaluate(&prev, &[], &[ro("r1", false)], &[], 30_000, false);
        assert_eq!(a, vec![offline("r1", OfflineSince::Unknown)]);
        assert_eq!(s.offline_since["r1"], OfflineSince::Unknown);
    }

    #[test]
    fn gap_of_exactly_stale_limit_is_not_stale() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(1_000);
        let (_, a, _) = evaluate(&prev, &[], &[ro("r1", false)], &[], 1_900, false);
        assert_eq!(a, vec![offline("r1", OfflineSince::Observed(500))]);
    }

    #[test]
    fn clock_rollback_never_leaves_a_future_offline_start() {
        // 系統時鐘被調回去：持久化的 Observed(t) 在「未來」，不能顯示成負的離線時間
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(5_000));
        prev.last_observed_at = Some(5_100);
        let (s, a, _) = evaluate(&prev, &[], &[ro("r1", false)], &[], 1_000, false);
        assert_eq!(a, vec![offline("r1", OfflineSince::Unknown)]);
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
        let (_, a, _) = evaluate(&prev, &[], &[ro("r1", false)], &[], 1_000, false);
        assert_eq!(a, vec![offline("r1", OfflineSince::Unknown)]);
    }

    #[test]
    fn observed_start_in_the_future_alone_is_downgraded() {
        // last_observed_at 看起來正常，但某筆 Observed(t) 的 t 在未來
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(2_000));
        prev.last_observed_at = Some(900);
        let (_, a, _) = evaluate(&prev, &[], &[ro("r1", false)], &[], 1_000, false);
        assert_eq!(a, vec![offline("r1", OfflineSince::Unknown)]);
    }

    #[test]
    fn recovery_clears_and_emits_event() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Observed(500));
        prev.last_observed_at = Some(900);
        let prev_att = [offline("r1", OfflineSince::Observed(500))];
        let (s, a, ev) = evaluate(&prev, &prev_att, &[ro("r1", true)], &[], 1_000, false);
        assert!(a.is_empty());
        assert!(s.offline_since.is_empty());
        assert_eq!(
            ev,
            vec![AttentionEvent::RunnerRecovered { key: "r1".into() }]
        );
    }

    #[test]
    fn removed_runner_is_dropped_without_recovered_event() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Unknown);
        prev.last_observed_at = Some(900);
        let prev_att = [offline("r1", OfflineSince::Unknown)];
        let (s, a, ev) = evaluate(&prev, &prev_att, &[], &[], 1_000, false);
        assert!(a.is_empty() && ev.is_empty());
        assert!(s.offline_since.is_empty());
    }

    #[test]
    fn still_offline_emits_no_repeat_event() {
        let mut prev = ObservationState::default();
        prev.offline_since
            .insert("r1".into(), OfflineSince::Unknown);
        prev.last_observed_at = Some(900);
        let prev_att = [offline("r1", OfflineSince::Unknown)];
        let (_, a, ev) = evaluate(&prev, &prev_att, &[ro("r1", false)], &[], 1_000, false);
        assert_eq!(a.len(), 1);
        assert!(ev.is_empty());
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
