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
/// Blacksmith 的機型標籤與 VM 名稱都長這樣：`blacksmith-<N>vcpu-…`（例：`blacksmith-8vcpu-ubuntu-2404`、
/// VM 名稱 `blacksmith-2vcpu-ubuntu-2404-<id>`）。只比 `blacksmith-` 前綴會太寬：一個不相干的
/// `blacksmith-cache` 標籤，或一台真的叫 `blacksmith-builder` 的自家 runner，都不該被當成雲端 VM。
const BLACKSMITH_PREFIX: &str = "blacksmith-";

/// 一個標籤或 VM 名稱是不是 Blacksmith 的機型命名（`blacksmith-<N>vcpu-…`，不分大小寫）。
pub fn is_blacksmith_machine(s: &str) -> bool {
    // `get` 而不是 `s[..n]`：標籤與名稱是 GitHub 回來的任意字串，位元組位移落在多位元組字元
    // 中間時切片會 panic，整個快照組裝就停了。
    let Some(prefix) = s.get(..BLACKSMITH_PREFIX.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(BLACKSMITH_PREFIX) {
        return false;
    }
    // 前綴是 ASCII 且已比對相等，所以這個位移一定在字元邊界上。
    let rest = &s[BLACKSMITH_PREFIX.len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && rest[digits..]
            .get(.."vcpu-".len())
            .is_some_and(|p| p.eq_ignore_ascii_case("vcpu-"))
}

/// 這個 job 是不是跑在（或要跑在）Blacksmith 的一次性 VM 上。
/// 看 labels（排隊時就看得到）與 runner 名稱（已在跑時）。Blacksmith 不是 GitHub-hosted、
/// 也不是自家 runner：不進 self-hosted 名冊，不參與排隊對位，也不算排隊太久。
pub fn is_blacksmith(job: &JobFacts) -> bool {
    job.labels.iter().any(|l| is_blacksmith_machine(l))
        || job
            .runner_name
            .as_deref()
            .is_some_and(is_blacksmith_machine)
}

/// Blacksmith VM 也會出現在 org 的 runner 名冊（名稱 `blacksmith-…`、labels 常為空），
/// 它們是用完即丟的雲端機器，不是自家 runner：不該被當成「離線的 runner」。
pub fn is_blacksmith_runner(runner: &RunnerFacts) -> bool {
    is_blacksmith_machine(&runner.name)
}

/// 這個 job 是不是跑在 self-hosted runner 上。
/// - 跑在 `GitHub Actions` group 的一律不是（即使 labels 含 self-hosted）
/// - labels 含 `self-hosted`（不分大小寫）是
/// - 已在跑、且 group 名稱非空（又不是 hosted）也是；null／空字串不算證據
/// - Blacksmith 的 job（`is_blacksmith`）一律不是
pub fn needs_self_hosted(job: &JobFacts) -> bool {
    // 先於其他規則：Blacksmith 上已在跑的 job 有非空的 runner group 名稱，會被下面
    // 「已在跑、group 非空」那條誤判成自家 runner。
    if is_blacksmith(job) {
        return false;
    }
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

    fn runner_named(name: &str) -> RunnerFacts {
        runner(name, true, false, &[], all())
    }

    #[test]
    fn queued_blacksmith_job_is_not_self_hosted_and_has_no_candidates() {
        let j = job(&["blacksmith-8vcpu-ubuntu-2404"], "o/r");
        assert!(is_blacksmith(&j));
        assert!(!needs_self_hosted(&j));
        let r = runner("mini", true, false, &["self-hosted", "blacksmith-8vcpu-ubuntu-2404"], all());
        assert!(candidates(&j, &[r]).is_empty(), "不參與排隊對位");
    }

    #[test]
    fn running_blacksmith_job_with_a_group_name_is_not_self_hosted() {
        // 這是原本的缺口：已在跑、group 非空 → 被當成自家 runner，補出一列假的忙碌 runner。
        let mut j = job(&["blacksmith-8vcpu-ubuntu-2404"], "o/r");
        j.state = JobState::InProgress;
        j.runner_group_name = Some("Default".into());
        j.runner_name = Some("blacksmith-8vcpu-ubuntu-2404-abc".into());
        assert!(!needs_self_hosted(&j));
    }

    #[test]
    fn blacksmith_is_recognised_from_the_runner_name_alone() {
        let mut j = job(&[], "o/r");
        j.state = JobState::InProgress;
        j.runner_group_name = Some("Default".into());
        j.runner_name = Some("Blacksmith-2vcpu-ubuntu-2404-xyz".into());
        assert!(is_blacksmith(&j), "名稱不分大小寫");
        assert!(!needs_self_hosted(&j));
    }

    #[test]
    fn blacksmith_label_is_matched_case_insensitively() {
        assert!(is_blacksmith(&job(&["Blacksmith-4vcpu-ubuntu-2404"], "o/r")));
    }

    #[test]
    fn self_hosted_jobs_and_lookalikes_are_not_blacksmith() {
        assert!(!is_blacksmith(&job(&["self-hosted", "Linux", "ci-burst-eligible"], "o/r")));
        assert!(needs_self_hosted(&job(&["self-hosted", "ci-burst-eligible"], "o/r")));
        // 只是名稱裡有 blacksmith 不算：必須是 `blacksmith-<N>vcpu-` 的機型命名
        assert!(!is_blacksmith(&job(&["my-blacksmith-runner"], "o/r")));
        assert!(!is_blacksmith(&job(&["black"], "o/r")), "比前綴短的標籤不會 panic");
        // 同前綴但不是機型：一個自家 job 帶著不相干的標籤，仍是自家 job
        let j = job(&["self-hosted", "blacksmith-cache"], "o/r");
        assert!(!is_blacksmith(&j));
        assert!(needs_self_hosted(&j));
        assert!(!is_blacksmith(&job(&["blacksmith-vcpu-x"], "o/r")), "沒有數字不是機型");
        assert!(!is_blacksmith(&job(&["blacksmith-8cpu-x"], "o/r")), "不是 vcpu 不是機型");
    }

    #[test]
    fn non_ascii_labels_and_names_never_panic() {
        // 位元組位移 11 落在多位元組字元中間：舊寫法 `s[..11]` 會 panic
        let tricky = "ééééééé"; // 14 bytes, offset 11 is inside a character
        assert!(!is_blacksmith(&job(&[tricky], "o/r")));
        assert!(!is_blacksmith_runner(&runner_named(tricky)));
        let mut j = job(&[], "o/r");
        j.runner_name = Some("blacksmith-é".into());
        assert!(!is_blacksmith(&j));
        assert!(is_blacksmith(&job(&["blacksmith-8vcpu-é"], "o/r")), "機型前綴之後可以是任何字串");
    }

    #[test]
    fn a_real_runner_that_merely_starts_with_blacksmith_stays_in_the_roster() {
        assert!(!is_blacksmith_runner(&runner_named("blacksmith-builder")));
        assert!(!is_blacksmith_runner(&runner("blacksmith-builder", true, false, &["self-hosted"], all())));
    }

    #[test]
    fn blacksmith_vms_are_recognised_in_the_org_runner_roster() {
        assert!(is_blacksmith_runner(&runner_named("blacksmith-2vcpu-ubuntu-2404-56i2b5l67zytz24p")));
        assert!(!is_blacksmith_runner(&runner_named("netcup-debian13-vps")));
        assert!(!is_blacksmith_runner(&runner_named("blacksmith")), "沒有連字號的名稱不是 VM");
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
