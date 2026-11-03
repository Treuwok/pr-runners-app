//! 排隊對位：這個排隊中的 job，哪些 online 的 runner 接得起來。
//! 只比 labels 與 group 對 repo 的授權；GitHub 不告訴我們排隊 job 指定了哪個 group
//! （第一版限制，見 spec §9），所以「能接」不是 GitHub 實際派工的保證。

use crate::model::{GroupAccess, JobFacts, JobState, RunnerFacts};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 宣告順序就是排序順序：閒置能接 → 忙碌能接 → 不確定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum CandidateKind {
    CanTakeIdle,
    CanTakeBusy,
    /// group 資訊不明，或 group 限定了 workflow：可能能接。
    Maybe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Candidate {
    pub runner_key: String,
    pub runner_name: String,
    pub kind: CandidateKind,
}

const SELF_HOSTED: &str = "self-hosted";
const HOSTED_GROUP: &str = "GitHub Actions";

/// 這個 job 是不是跑在 self-hosted runner 上。
/// - 跑在 `GitHub Actions` group 的一律不是（即使 labels 含 self-hosted）
/// - labels 含 `self-hosted`（不分大小寫）是
/// - 已在跑、且 group 名稱非空（又不是 hosted）也是；null／空字串不算證據
pub fn needs_self_hosted(job: &JobFacts) -> bool {
    let group = job.runner_group_name.as_deref().unwrap_or("");
    if group == HOSTED_GROUP {
        return false;
    }
    if job
        .labels
        .iter()
        .any(|l| l.eq_ignore_ascii_case(SELF_HOSTED))
    {
        return true;
    }
    job.state == JobState::InProgress && !group.is_empty()
}

/// 呼叫端只傳**同一個 scope** 的 runner（不跨 org，repo 層級 runner 由 `Selected` 限定自己的 repo）。
/// 回傳只含 online 的 runner，依「閒置能接 → 忙碌能接 → 不確定」再依名稱排序。
pub fn candidates(job: &JobFacts, runners: &[RunnerFacts]) -> Vec<Candidate> {
    if !needs_self_hosted(job) {
        return Vec::new();
    }
    let needed: BTreeSet<String> = job.labels.iter().map(|l| l.to_ascii_lowercase()).collect();
    let mut out: Vec<Candidate> = runners
        .iter()
        .filter(|r| r.online)
        .filter(|r| {
            let have: BTreeSet<String> = r.labels.iter().map(|l| l.to_ascii_lowercase()).collect();
            needed.is_subset(&have)
        })
        .filter_map(|r| {
            let certain = match access_for(&r.group_access, job) {
                Access::Denied => return None,
                Access::Allowed => !r.restricted_to_workflows,
                Access::Unknown => false,
            };
            let kind = match (certain, r.busy) {
                (false, _) => CandidateKind::Maybe,
                (true, false) => CandidateKind::CanTakeIdle,
                (true, true) => CandidateKind::CanTakeBusy,
            };
            Some(Candidate {
                runner_key: r.key.clone(),
                runner_name: r.name.clone(),
                kind,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.runner_name.cmp(&b.runner_name))
            .then_with(|| a.runner_key.cmp(&b.runner_key))
    });
    out
}

enum Access {
    Allowed,
    Denied,
    Unknown,
}

/// group 允不允許這個 job 的 repo：已知不允許就是 Denied（不會被降成「可能」）。
fn access_for(access: &GroupAccess, job: &JobFacts) -> Access {
    let public_gate = |public_ok: bool| {
        if job.repo_public && !public_ok {
            Access::Denied
        } else {
            Access::Allowed
        }
    };
    match access {
        GroupAccess::All { public_ok } => public_gate(*public_ok),
        GroupAccess::Private if job.repo_public => Access::Denied,
        GroupAccess::Private => Access::Allowed,
        GroupAccess::Selected { repos, public_ok } => {
            // GitHub 的 repo 全名不分大小寫
            if repos.iter().any(|r| r.eq_ignore_ascii_case(&job.repo)) {
                public_gate(*public_ok)
            } else {
                Access::Denied
            }
        }
        GroupAccess::Unknown => Access::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(
        name: &str,
        online: bool,
        busy: bool,
        labels: &[&str],
        access: GroupAccess,
    ) -> RunnerFacts {
        RunnerFacts {
            key: format!("1/acme/{name}"),
            id: 1,
            name: name.into(),
            online,
            busy,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            group_access: access,
            restricted_to_workflows: false,
        }
    }

    fn job_in(labels: &[&str], repo: &str, public: bool) -> JobFacts {
        JobFacts {
            key: "10/1".into(),
            repo: repo.into(),
            repo_public: public,
            state: JobState::Queued,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            runner_id: None,
            runner_name: None,
            runner_group_name: None,
            created_at: 0,
            started_at: None,
        }
    }
    fn job(labels: &[&str], repo: &str) -> JobFacts {
        job_in(labels, repo, false)
    }
    fn job_public(labels: &[&str], repo: &str) -> JobFacts {
        job_in(labels, repo, true)
    }
    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn cand(name: &str, kind: CandidateKind) -> Candidate {
        Candidate {
            runner_key: format!("1/acme/{name}"),
            runner_name: name.into(),
            kind,
        }
    }
    fn all() -> GroupAccess {
        GroupAccess::All { public_ok: true }
    }

    #[test]
    fn labels_must_be_subset_case_insensitive() {
        let r = runner(
            "mini",
            true,
            false,
            &["self-hosted", "macOS", "ARM64"],
            all(),
        );
        let j = job(&["self-hosted", "macos", "arm64"], "Acme/acme-web");
        assert_eq!(
            candidates(&j, &[r]),
            vec![cand("mini", CandidateKind::CanTakeIdle)]
        );
    }

    #[test]
    fn missing_label_excludes_runner() {
        let r = runner("vps", true, false, &["self-hosted", "Linux", "X64"], all());
        assert!(candidates(&job(&["self-hosted", "macOS"], "o/r"), &[r]).is_empty());
    }

    #[test]
    fn busy_runner_is_can_take_busy() {
        let r = runner("mini", true, true, &["self-hosted"], all());
        assert_eq!(
            candidates(&job(&["self-hosted"], "o/r"), &[r]),
            vec![cand("mini", CandidateKind::CanTakeBusy)]
        );
    }

    #[test]
    fn selected_group_without_repo_excludes() {
        let r = runner(
            "light",
            true,
            false,
            &["self-hosted"],
            GroupAccess::Selected {
                repos: set(&["Acme/acme-app"]),
                public_ok: false,
            },
        );
        assert!(candidates(&job(&["self-hosted"], "Acme/acme-book"), &[r]).is_empty());
    }

    #[test]
    fn selected_group_with_repo_is_candidate_ignoring_name_case() {
        let r = runner(
            "light",
            true,
            false,
            &["self-hosted"],
            GroupAccess::Selected {
                repos: set(&["Acme/Acme-Book"]),
                public_ok: false,
            },
        );
        assert_eq!(
            candidates(&job(&["self-hosted"], "acme/acme-book"), &[r]),
            vec![cand("light", CandidateKind::CanTakeIdle)]
        );
    }

    #[test]
    fn unknown_group_is_maybe() {
        let r = runner("x", true, false, &["self-hosted"], GroupAccess::Unknown);
        assert_eq!(
            candidates(&job(&["self-hosted"], "o/r"), &[r]),
            vec![cand("x", CandidateKind::Maybe)]
        );
    }

    #[test]
    fn public_repo_excluded_when_group_disallows_public() {
        // Review Focus 6
        let r = runner(
            "mini",
            true,
            false,
            &["self-hosted"],
            GroupAccess::All { public_ok: false },
        );
        assert!(candidates(&job_public(&["self-hosted"], "Acme/acme-tree"), &[r]).is_empty());
    }

    #[test]
    fn public_repo_allowed_when_group_allows_public() {
        let r = runner("mini", true, false, &["self-hosted"], all());
        assert_eq!(
            candidates(&job_public(&["self-hosted"], "o/r"), &[r]),
            vec![cand("mini", CandidateKind::CanTakeIdle)]
        );
    }

    #[test]
    fn private_group_serves_private_repo_only() {
        let r = runner("p", true, false, &["self-hosted"], GroupAccess::Private);
        assert_eq!(
            candidates(&job(&["self-hosted"], "o/r"), std::slice::from_ref(&r)),
            vec![cand("p", CandidateKind::CanTakeIdle)]
        );
        assert!(candidates(&job_public(&["self-hosted"], "o/r"), &[r]).is_empty());
    }

    #[test]
    fn repo_level_runner_only_serves_its_repo() {
        let r = runner(
            "solo",
            true,
            false,
            &["self-hosted"],
            GroupAccess::Selected {
                repos: set(&["o/x"]),
                public_ok: true,
            },
        );
        assert!(candidates(&job(&["self-hosted"], "o/y"), std::slice::from_ref(&r)).is_empty());
        assert_eq!(
            candidates(&job(&["self-hosted"], "o/x"), &[r]),
            vec![cand("solo", CandidateKind::CanTakeIdle)]
        );
    }

    #[test]
    fn restricted_to_workflows_downgrades_known_ok_to_maybe() {
        let mut r = runner("r", true, false, &["self-hosted"], all());
        r.restricted_to_workflows = true;
        assert_eq!(
            candidates(&job(&["self-hosted"], "o/r"), &[r]),
            vec![cand("r", CandidateKind::Maybe)]
        );
    }

    #[test]
    fn restricted_to_workflows_never_rescues_known_denied() {
        let mut r = runner(
            "r",
            true,
            false,
            &["self-hosted"],
            GroupAccess::Selected {
                repos: set(&["o/other"]),
                public_ok: true,
            },
        );
        r.restricted_to_workflows = true;
        assert!(candidates(&job(&["self-hosted"], "o/r"), &[r]).is_empty());
    }

    #[test]
    fn offline_runner_never_candidate() {
        let r = runner("off", false, false, &["self-hosted"], all());
        assert!(candidates(&job(&["self-hosted"], "o/r"), &[r]).is_empty());
    }

    #[test]
    fn results_sorted_idle_then_busy_then_maybe_then_name() {
        let rs = [
            runner(
                "z-maybe",
                true,
                false,
                &["self-hosted"],
                GroupAccess::Unknown,
            ),
            runner("b-busy", true, true, &["self-hosted"], all()),
            runner("z-idle", true, false, &["self-hosted"], all()),
            runner("a-idle", true, false, &["self-hosted"], all()),
        ];
        let names: Vec<String> = candidates(&job(&["self-hosted"], "o/r"), &rs)
            .into_iter()
            .map(|c| c.runner_name)
            .collect();
        assert_eq!(names, ["a-idle", "z-idle", "b-busy", "z-maybe"]);
    }

    #[test]
    fn hosted_job_is_not_self_hosted() {
        assert!(!needs_self_hosted(&job(&["ubuntu-latest"], "o/r")));
    }

    #[test]
    fn hosted_job_never_gets_candidates() {
        let r = runner("any", true, false, &[], all());
        assert!(candidates(&job(&["ubuntu-latest"], "o/r"), &[r]).is_empty());
    }

    #[test]
    fn self_hosted_label_is_case_insensitive() {
        assert!(needs_self_hosted(&job(&["Self-Hosted"], "o/r")));
    }

    #[test]
    fn hosted_group_name_is_not_self_hosted() {
        let mut j = job(&["self-hosted"], "o/r");
        j.runner_group_name = Some("GitHub Actions".into());
        assert!(!needs_self_hosted(&j));
    }

    #[test]
    fn running_job_with_real_group_is_self_hosted_without_label() {
        let mut j = job(&["custom"], "o/r");
        j.state = JobState::InProgress;
        j.runner_group_name = Some("Default".into());
        assert!(needs_self_hosted(&j));
    }

    #[test]
    fn null_or_empty_group_is_no_evidence() {
        let mut j = job(&["custom"], "o/r");
        j.state = JobState::InProgress;
        assert!(!needs_self_hosted(&j));
        j.runner_group_name = Some(String::new());
        assert!(!needs_self_hosted(&j));
    }

    #[test]
    fn queued_job_with_group_name_but_no_label_is_not_self_hosted() {
        // 群組名稱只對「已在跑」的 job 當證據
        let mut j = job(&["custom"], "o/r");
        j.runner_group_name = Some("Default".into());
        assert!(!needs_self_hosted(&j));
    }
}
