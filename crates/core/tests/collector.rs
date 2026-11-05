//! 收集器整合測試：wiremock 當 GitHub，驗證 Review Focus 2/3/5 與各降級路徑。

use pr_runners_core::persist::{AccountKind, AccountMeta};
use pr_runners_core::runlog::{JobOutcome, RecordSource};
use pr_runners_core::view::*;
use pr_runners_core::{Collector, GitHubClient, Hold, TickMode};
use pr_runners_rules::{CandidateKind, JobState, OfflineSince};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const T0: i64 = 1_800_000_000;
thread_local! { static NOW: Cell<i64> = const { Cell::new(T0) }; }
fn now() -> i64 {
    NOW.with(|n| n.get())
}
fn set_now(t: i64) {
    NOW.with(|n| n.set(t));
}
fn iso(t: i64) -> String {
    OffsetDateTime::from_unix_timestamp(t)
        .unwrap()
        .format(&Rfc3339)
        .unwrap()
}

// ── GitHub 回應的小建構函式 ──
fn runner(id: u64, name: &str, online: bool, busy: bool, labels: &[&str]) -> Value {
    json!({"id": id, "name": name, "os": "Linux", "status": if online {"online"} else {"offline"},
           "busy": busy, "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>()})
}
fn repo(full: &str, kind: &str, pushed_at: i64, private: bool) -> Value {
    let owner = full.split('/').next().unwrap();
    json!({"full_name": full, "private": private, "pushed_at": iso(pushed_at),
           "owner": {"login": owner, "type": kind}})
}
fn run_json(id: u64, pr: Option<u64>) -> Value {
    json!({"id": id, "name": "ci", "event": "pull_request", "head_branch": "feat/x", "head_sha": "sha",
           "pull_requests": pr.map(|n| vec![json!({"number": n})]).unwrap_or_default(),
           "html_url": format!("https://github.com/o/r/actions/runs/{id}")})
}
#[allow(clippy::too_many_arguments)]
fn job(
    id: u64,
    run_id: u64,
    name: &str,
    status: &str,
    labels: &[&str],
    runner: Option<(u64, &str, &str)>,
    created: i64,
    started: Option<i64>,
) -> Value {
    json!({"id": id, "run_id": run_id, "name": name, "status": status, "labels": labels,
           "runner_id": runner.map(|r| r.0), "runner_name": runner.map(|r| r.1), "runner_group_name": runner.map(|r| r.2),
           "created_at": iso(created), "started_at": started.map(iso), "workflow_name": "ci",
           "run_attempt": 1, "html_url": format!("https://github.com/o/r/actions/runs/{run_id}/job/{id}")})
}

async fn mount(server: &MockServer, p: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
async fn mount_status(server: &MockServer, p: &str, status: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .and(query_param("status", status))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
async fn mount_code(server: &MockServer, p: &str, code: u16) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(code).set_body_json(json!({"message": "x"})))
        .mount(server)
        .await;
}
/// 沒另外指定的 runs 一律回空（優先序最低，讓具體的 mock 先中）。
async fn empty_runs_fallback(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/repos/[^/]+/[^/]+/actions/runs$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"total_count": 0, "workflow_runs": []})),
        )
        .with_priority(10)
        .mount(server)
        .await;
}
async fn empty_groups(server: &MockServer, org: &str) {
    mount(
        server,
        &format!("/orgs/{org}/actions/runner-groups"),
        json!({"total_count": 0, "runner_groups": []}),
    )
    .await;
}

fn client(server: &MockServer) -> GitHubClient {
    GitHubClient::new(Url::parse(&server.uri()).unwrap(), "tok".into())
}
fn app_account(id: u64, login: &str, scopes: &[&str]) -> AccountMeta {
    AccountMeta {
        account_id: id,
        login: login.into(),
        kind: AccountKind::GitHubApp,
        selected_scopes: scopes.iter().map(|s| s.to_string()).collect(),
    }
}
fn pat_account(id: u64, login: &str, scopes: &[&str]) -> AccountMeta {
    AccountMeta {
        kind: AccountKind::Pat,
        ..app_account(id, login, scopes)
    }
}
fn collector(accounts: Vec<(AccountMeta, GitHubClient)>) -> Collector {
    Collector::new(accounts, BTreeMap::new(), 2_500, now)
}
fn scope<'a>(snap: &'a Snapshot, acct: usize, name: &str) -> &'a ScopeView {
    snap.accounts[acct]
        .scopes
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("沒有 scope {name}: {:?}", snap.accounts[acct]))
}
fn runner_named<'a>(s: &'a ScopeView, name: &str) -> &'a RunnerView {
    s.runners
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("沒有 runner {name}"))
}
async fn count(server: &MockServer, needle: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.to_string().contains(needle))
        .count()
}

#[tokio::test]
async fn same_runner_name_in_two_orgs_is_not_confused() {
    // Review Focus 2：A、B 都有 builder-1（連 runner id 都相同），只有 A 的 job 跑在上面
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"total_count": 2, "installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}},
        {"id": 2, "account": {"login": "B", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/2/repositories",
        json!({"repositories": [repo("B/rb", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(5, "builder-1", true, true, &["self-hosted"])]}),
    )
    .await;
    mount(
        &s,
        "/orgs/B/actions/runners",
        json!({"runners": [runner(5, "builder-1", true, false, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&s, "A").await;
    empty_groups(&s, "B").await;
    mount_status(
        &s,
        "/repos/A/ra/actions/runs",
        "in_progress",
        json!({"workflow_runs": [run_json(100, Some(3))]}),
    )
    .await;
    mount(&s, "/repos/A/ra/actions/runs/100/jobs", json!({"jobs": [
        job(1, 100, "build", "in_progress", &["self-hosted"], Some((5, "builder-1", "Default")), T0 - 300, Some(T0 - 120))]})).await;
    empty_runs_fallback(&s).await;

    let mut c = collector(vec![(app_account(1, "me", &["A", "B"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    let (a, b) = (scope(&snap, 0, "A"), scope(&snap, 0, "B"));
    let (ra, rb) = (runner_named(a, "builder-1"), runner_named(b, "builder-1"));
    assert_eq!(ra.state, RunnerState::Busy);
    assert_eq!(ra.job.as_ref().unwrap().pr_tag, "PR#3");
    assert_eq!(
        ra.elapsed,
        Some(pr_runners_rules::Elapsed::Known { secs: 120 })
    );
    assert_eq!(
        rb.state,
        RunnerState::Idle,
        "B 的同名 runner 不能被 A 的 job 標成忙碌"
    );
    assert!(rb.job.is_none());
    assert_ne!(ra.key, rb.key);
    assert_eq!(
        (snap.summary.own_busy, snap.summary.own_online),
        (1, 2)
    );
}

#[tokio::test]
async fn revoked_token_only_marks_that_account() {
    // Review Focus 3
    set_now(T0);
    let (bad, good) = (MockServer::start().await, MockServer::start().await);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&bad)
        .await;
    mount(
        &good,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "G", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &good,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("G/r", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &good,
        "/orgs/G/actions/runners",
        json!({"runners": [runner(1, "g1", true, false, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&good, "G").await;
    empty_runs_fallback(&good).await;

    let mut c = collector(vec![
        (app_account(1, "bad", &["X"]), client(&bad)),
        (app_account(2, "good", &["G"]), client(&good)),
    ]);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(snap.accounts[0].status, AccountStatus::NeedsReauth);
    assert_eq!(snap.accounts[1].status, AccountStatus::Ok);
    assert_eq!(
        runner_named(scope(&snap, 1, "G"), "g1").state,
        RunnerState::Idle
    );
}

#[tokio::test]
async fn installation_404_means_needs_reauth() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "G", "type": "Organization"}}]}),
    )
    .await;
    mount_code(&s, "/user/installations/1/repositories", 404).await;
    let mut c = collector(vec![(app_account(1, "u", &["G"]), client(&s))]);
    assert_eq!(
        c.tick(TickMode::Full).await.accounts[0].status,
        AccountStatus::NeedsReauth
    );
}

#[tokio::test]
async fn user_scope_with_no_runners_full_scan_budget() {
    // Review Focus 5：53 個 repo 的帳號、全都沒有 runner 和 run
    set_now(T0);
    let s = MockServer::start().await;
    let repos: Vec<Value> = (0..53)
        .map(|i| repo(&format!("jdoe/r{i}"), "User", T0 - 90 * 86_400, false))
        .collect();
    mount(&s, "/user/repos", json!(repos)).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/repos/jdoe/r\d+/actions/runners$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"total_count": 0, "runners": []})),
        )
        .mount(&s)
        .await;
    empty_runs_fallback(&s).await;

    let mut c = collector(vec![(
        pat_account(9, "jdoe", &["jdoe"]),
        client(&s),
    )]);
    let snap = c.tick(TickMode::Full).await;
    let requests = s.received_requests().await.unwrap().len();
    assert!(requests <= 53 * 4 + 2, "Full tick 用了 {requests} 次請求");
    assert_eq!(
        requests,
        1 + 53 * 4,
        "1 次 repo 清單 ＋ 每個 repo 4 次（repo runner ＋ 三種狀態的 runs）"
    );
    let sc = scope(&snap, 0, "jdoe");
    assert!(sc.runners.is_empty() && sc.waiting.is_empty() && sc.jobs.is_empty());
    assert_eq!(sc.roster, RosterLevel::Full);
    assert_eq!(snap.accounts[0].status, AccountStatus::Ok);
    assert_eq!(snap.summary.attention, Severity::Ok);
}

#[tokio::test]
async fn forbidden_roster_degrades_to_busy_only() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "O", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("O/r", "Organization", T0, true)]}),
    )
    .await;
    mount_code(&s, "/orgs/O/actions/runners", 403).await;
    mount_status(
        &s,
        "/repos/O/r/actions/runs",
        "in_progress",
        json!({"workflow_runs": [run_json(100, Some(1)), run_json(101, Some(1))]}),
    )
    .await;
    mount(&s, "/repos/O/r/actions/runs/100/jobs", json!({"jobs": [
        job(1, 100, "build", "in_progress", &["self-hosted", "macOS"], Some((7, "mac1", "Default")), T0 - 100, Some(T0 - 60))]})).await;
    // 同時跑著的 hosted job 不能出現在名冊
    mount(&s, "/repos/O/r/actions/runs/101/jobs", json!({"jobs": [
        job(2, 101, "lint", "in_progress", &["ubuntu-latest"], Some((1_000_000_003, "GitHub Actions 3", "GitHub Actions")), T0 - 100, Some(T0 - 60))]})).await;
    empty_runs_fallback(&s).await;

    let mut c = collector(vec![(app_account(1, "u", &["O"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    let sc = scope(&snap, 0, "O");
    assert_eq!(sc.roster, RosterLevel::BusyOnly);
    let names: Vec<_> = sc.runners.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["mac1"]);
    assert_eq!(sc.runners[0].state, RunnerState::Busy);
    assert_eq!(sc.jobs.len(), 2, "hosted job 仍在 Jobs 清單，標 hosted");
    assert!(sc.jobs.iter().find(|j| j.job == "lint").unwrap().hosted);
    assert!(!sc.jobs.iter().find(|j| j.job == "build").unwrap().hosted);
    // 之後的一般輪詢不再每輪重打被拒的名冊
    set_now(T0 + 30);
    c.tick(TickMode::Incremental).await;
    assert_eq!(count(&s, "/orgs/O/actions/runners").await, 1);
}

#[tokio::test]
async fn busy_only_without_runner_id_uses_repo_and_name_and_never_merges_across_repos() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "O", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [
        repo("O/a", "Organization", T0, true), repo("O/b", "Organization", T0, true)]}),
    )
    .await;
    mount_code(&s, "/orgs/O/actions/runners", 403).await;
    for (r, run) in [("a", 100), ("b", 200)] {
        mount_status(
            &s,
            &format!("/repos/O/{r}/actions/runs"),
            "in_progress",
            json!({"workflow_runs": [run_json(run, Some(1))]}),
        )
        .await;
        // runner_id 為 0（尚未指派／拿不到 id），名稱相同
        let mut j = job(
            run,
            run,
            "t",
            "in_progress",
            &["self-hosted"],
            Some((0, "box", "Default")),
            T0 - 50,
            Some(T0 - 40),
        );
        j["runner_id"] = json!(0);
        mount(
            &s,
            &format!("/repos/O/{r}/actions/runs/{run}/jobs"),
            json!({"jobs": [j]}),
        )
        .await;
    }
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "u", &["O"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(
        scope(&snap, 0, "O").runners.len(),
        2,
        "不同 repo 的同名降級識別不能合併"
    );
}

#[tokio::test]
async fn scope_observations_are_independent() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}},
        {"id": 2, "account": {"login": "bob", "type": "User"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    // bob 有一個冷 repo，上面有一台離線的 repo runner
    mount(
        &s,
        "/user/installations/2/repositories",
        json!({"repositories": [repo("bob/cold", "User", T0 - 90 * 86_400, true)]}),
    )
    .await;
    mount(
        &s,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(1, "r1", false, false, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&s, "A").await;
    mount(
        &s,
        "/repos/bob/cold/actions/runners",
        json!({"runners": [runner(9, "bob-box", false, false, &["self-hosted"])]}),
    )
    .await;
    empty_runs_fallback(&s).await;

    let mut c = collector(vec![(app_account(1, "me", &["A", "bob"]), client(&s))]);
    let t1 = c.tick(TickMode::Full).await;
    let first =
        |snap: &Snapshot, sc: &str, n: &str| runner_named(scope(snap, 0, sc), n).state.clone();
    assert_eq!(
        first(&t1, "A", "r1"),
        RunnerState::Offline {
            since: OfflineSince::Unknown
        }
    );
    assert_eq!(
        first(&t1, "bob", "bob-box"),
        RunnerState::Offline {
            since: OfflineSince::Unknown
        }
    );
    let states1 = c.states();
    assert!(
        states1["1/A"]
            .offline_since
            .values()
            .any(|v| *v == OfflineSince::Unknown)
    );

    // 一般輪詢：A 的 r1 仍保有第一輪記下的離線起點（沒被 bob 的空輸入清掉）；
    // bob 的冷 repo 沒被重掃，那台 runner 仍在名冊上、離線起點不變
    set_now(T0 + 130);
    let t2 = c.tick(TickMode::Incremental).await;
    assert_eq!(
        first(&t2, "A", "r1"),
        RunnerState::Offline {
            since: OfflineSince::Unknown
        }
    );
    assert_eq!(
        first(&t2, "bob", "bob-box"),
        RunnerState::Offline {
            since: OfflineSince::Unknown
        }
    );
    assert_eq!(
        c.states()["1/bob"],
        states1["1/bob"].clone_with_observed(T0 + 130)
    );
    assert_eq!(
        count(&s, "/repos/bob/cold/actions/runners").await,
        1,
        "冷 repo 在 600 秒內不重掃"
    );
}

trait WithObserved {
    fn clone_with_observed(&self, t: i64) -> Self;
}
impl WithObserved for pr_runners_rules::ObservationState {
    /// 只有 last_observed_at 會隨每次成功評估前進；其餘（離線起點）必須不變。
    fn clone_with_observed(&self, t: i64) -> Self {
        Self {
            last_observed_at: Some(t),
            ..self.clone()
        }
    }
}

#[tokio::test]
async fn failed_scope_keeps_its_observation_state() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}},
        {"id": 2, "account": {"login": "bob", "type": "User"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/2/repositories",
        json!({"repositories": [repo("bob/r", "User", T0, true)]}),
    )
    .await;
    mount(&s, "/orgs/A/actions/runners", json!({"runners": []})).await;
    empty_groups(&s, "A").await;
    mount(
        &s,
        "/repos/bob/r/actions/runners",
        json!({"runners": [runner(9, "bob-box", false, false, &["self-hosted"])]}),
    )
    .await;
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "me", &["A", "bob"]), client(&s))]);
    c.tick(TickMode::Full).await;
    let bob_before = c.states()["1/bob"].clone();

    // 之後 bob 的 repo 清單變成 502（網路類）：整個帳號 Offline，bob 的觀察狀態原樣保留
    s.reset().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}},
        {"id": 2, "account": {"login": "bob", "type": "User"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/2/repositories",
        json!({"repositories": [repo("bob/r", "User", T0, true)]}),
    )
    .await;
    mount(&s, "/orgs/A/actions/runners", json!({"runners": []})).await;
    empty_groups(&s, "A").await;
    mount_code(&s, "/repos/bob/r/actions/runners", 502).await;
    empty_runs_fallback(&s).await;
    set_now(T0 + 10);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(
        snap.accounts[0].status,
        AccountStatus::Offline { last_ok_at: T0 }
    );
    assert_eq!(c.states()["1/bob"], bob_before);
    assert!(!snap.accounts[0].scopes.is_empty(), "斷線時沿用上一份內容");
}

#[tokio::test]
async fn waiting_section_has_candidates() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "X", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("X/r", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/orgs/X/actions/runners",
        json!({"runners": [
        runner(22, "mac1", true, true, &["self-hosted", "macOS", "ARM64"]),
        runner(31, "vps", true, false, &["self-hosted", "Linux"])]}),
    )
    .await;
    mount(&s, "/orgs/X/actions/runner-groups", json!({"runner_groups": [
        {"id": 1, "name": "Default", "visibility": "all", "allows_public_repositories": false, "restricted_to_workflows": false}]})).await;
    mount(
        &s,
        "/orgs/X/actions/runner-groups/1/runners",
        json!({"runners": [{"id": 22}, {"id": 31}]}),
    )
    .await;
    mount_status(
        &s,
        "/repos/X/r/actions/runs",
        "queued",
        json!({"workflow_runs": [run_json(300, Some(9))]}),
    )
    .await;
    mount(
        &s,
        "/repos/X/r/actions/runs/300/jobs",
        json!({"jobs": [
        job(7, 300, "mac-build", "queued", &["self-hosted", "macOS"], None, T0 - 700, None)]}),
    )
    .await;
    empty_runs_fallback(&s).await;

    let mut c = collector(vec![(app_account(1, "u", &["X"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    let sc = scope(&snap, 0, "X");
    assert_eq!(sc.waiting.len(), 1);
    let w = &sc.waiting[0];
    assert_eq!(w.job.state, JobState::Queued);
    assert_eq!(
        w.job.labels,
        ["self-hosted", "macOS"],
        "等待中顯示需要的 labels"
    );
    assert_eq!(w.candidates.len(), 1);
    assert_eq!(w.candidates[0].runner_name, "mac1");
    assert_eq!(w.candidates[0].kind, CandidateKind::CanTakeBusy);
    assert_eq!(
        runner_named(sc, "mac1").can_take,
        Some(CandidateKind::CanTakeBusy)
    );
    assert_eq!(
        runner_named(sc, "vps").can_take,
        None,
        "labels 不符的 runner 不標"
    );
    assert_eq!(w.age, pr_runners_rules::Elapsed::Known { secs: 700 });
    assert!(w.attention, "排隊超過 600 秒");
    assert_eq!(
        (snap.summary.waiting, snap.summary.attention),
        (1, Severity::Warn)
    );
    assert_eq!(runner_named(sc, "mac1").group.as_deref(), Some("Default"));
}

#[tokio::test]
async fn waiting_job_never_becomes_attention_or_waiting_section() {
    // Review Focus 4
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "X", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("X/r", "Organization", T0, true)]}),
    )
    .await;
    mount(&s, "/orgs/X/actions/runners", json!({"runners": []})).await;
    empty_groups(&s, "X").await;
    mount_status(
        &s,
        "/repos/X/r/actions/runs",
        "in_progress",
        json!({"workflow_runs": [run_json(300, Some(9))]}),
    )
    .await;
    mount(
        &s,
        "/repos/X/r/actions/runs/300/jobs",
        json!({"jobs": [
        job(7, 300, "deploy", "waiting", &["self-hosted"], None, T0 - 5_000, None)]}),
    )
    .await;
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "u", &["X"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    let sc = scope(&snap, 0, "X");
    assert!(sc.waiting.is_empty());
    assert_eq!(sc.jobs.len(), 1, "仍出現在 Jobs 清單");
    assert_eq!(snap.summary.attention, Severity::Ok);
}

#[tokio::test]
async fn full_within_120s_runs_as_incremental() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "X", "type": "Organization"}}]}),
    )
    .await;
    mount(&s, "/user/installations/1/repositories", json!({"repositories": [
        repo("X/hot", "Organization", T0 - 60, true), repo("X/cold", "Organization", T0 - 30 * 86_400, true)]})).await;
    mount(&s, "/orgs/X/actions/runners", json!({"runners": []})).await;
    empty_groups(&s, "X").await;
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "u", &["X"]), client(&s))]);

    c.tick(TickMode::Full).await;
    assert_eq!(
        (
            count(&s, "/repos/X/hot/actions/runs").await,
            count(&s, "/repos/X/cold/actions/runs").await
        ),
        (3, 3)
    );
    set_now(T0 + 60);
    c.tick(TickMode::Full).await; // 距上次完整掃描 60 秒 → 只查熱 repo
    assert_eq!(
        (
            count(&s, "/repos/X/hot/actions/runs").await,
            count(&s, "/repos/X/cold/actions/runs").await
        ),
        (6, 3)
    );
    set_now(T0 + 121);
    c.tick(TickMode::Full).await; // 超過 120 秒 → 真的完整掃描
    assert_eq!(count(&s, "/repos/X/cold/actions/runs").await, 6);
}

#[tokio::test]
async fn resume_makes_next_tick_a_first_observation() {
    // Android 回前景：先前親眼看到的離線起點降為「時間不明」的前提是 first_observation；
    // 這裡驗證 resume() 之後新看到的離線 runner 立即記為 Unknown（而不是從現在起計時）
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "X", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("X/r", "Organization", T0, true)]}),
    )
    .await;
    empty_groups(&s, "X").await;
    empty_runs_fallback(&s).await;
    mount(
        &s,
        "/orgs/X/actions/runners",
        json!({"runners": [runner(1, "r1", true, false, &["self-hosted"])]}),
    )
    .await;
    let mut c = collector(vec![(app_account(1, "u", &["X"]), client(&s))]);
    c.tick(TickMode::Full).await;

    s.reset().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [{"id": 1, "account": {"login": "X", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("X/r", "Organization", T0, true)]}),
    )
    .await;
    empty_groups(&s, "X").await;
    empty_runs_fallback(&s).await;
    mount(
        &s,
        "/orgs/X/actions/runners",
        json!({"runners": [runner(1, "r1", false, false, &["self-hosted"])]}),
    )
    .await;

    set_now(T0 + 30);
    let seen = c.tick(TickMode::Incremental).await; // 沒 resume：親眼看到上線→離線，從現在計時
    assert_eq!(
        runner_named(scope(&seen, 0, "X"), "r1").state,
        RunnerState::Offline {
            since: OfflineSince::Observed(T0 + 30)
        }
    );

    // 換一個 collector 模擬「剛回前景」：同樣的事實，resume 後立即 Unknown
    let st = pr_runners_rules::ObservationState {
        last_observed_at: Some(T0),
        ..Default::default()
    };
    let states = BTreeMap::from([("1/X".to_string(), st)]);
    let mut c2 = Collector::new(
        vec![(app_account(1, "u", &["X"]), client(&s))],
        states,
        2_500,
        now,
    );
    c2.resume();
    let snap = c2.tick(TickMode::Full).await;
    assert_eq!(
        runner_named(scope(&snap, 0, "X"), "r1").state,
        RunnerState::Offline {
            since: OfflineSince::Unknown
        }
    );
}

// ───────── S1 審查批次新增 ─────────

#[tokio::test]
async fn low_quota_slows_polling_but_never_waits_for_the_hourly_reset() {
    // 剩餘量 < 20% 只拉長間隔；只有真的被限流（403／429）才等到 reset
    set_now(T0);
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-RateLimit-Limit", "5000")
                .insert_header("X-RateLimit-Remaining", "100")
                .insert_header("X-RateLimit-Reset", (T0 + 3_000).to_string().as_str())
                .set_body_json(json!([])),
        )
        .mount(&s)
        .await;
    let mut c = collector(vec![(pat_account(1, "u", &["u"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    assert!(matches!(
        snap.accounts[0].status,
        AccountStatus::Throttled { remaining: 100, .. }
    ));
    let d = c.next_delay(&snap).as_secs();
    assert_eq!(d, 240, "閒置 120 秒，配額低乘 2；不能等一小時");
}

#[tokio::test]
async fn real_rate_limit_waits_until_the_retry_time() {
    set_now(T0);
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1500"))
        .mount(&s)
        .await;
    // 限流等待用的是用戶端的真實時鐘，所以這個測試的收集器也用真實時鐘
    fn real_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
    let mut c = Collector::new(
        vec![(pat_account(1, "u", &["u"]), client(&s))],
        BTreeMap::new(),
        2_500,
        real_now,
    );
    let snap = c.tick(TickMode::Full).await;
    assert!(matches!(
        snap.accounts[0].status,
        AccountStatus::Throttled { .. }
    ));
    // 真的被限流：不受 600 秒上限截短
    assert!(c.next_delay(&snap).as_secs() > 1_000);
}

#[tokio::test]
async fn pr_tag_lookups_are_not_shared_across_accounts() {
    // 兩個帳號看同一個 repo／sha：各自用自己的 token 查一次，不能沿用另一個帳號查到的結果
    set_now(T0);
    let mut servers = Vec::new();
    for _ in 0..2 {
        let s = MockServer::start().await;
        mount(&s, "/user/repos", json!([repo("me/r", "User", T0, true)])).await;
        // 第二個帳號慢一點開始查 PR，確保第一個帳號先把結果放進（若有共用的）快取
        let delay = std::time::Duration::from_millis(if servers.is_empty() { 0 } else { 400 });
        Mock::given(method("GET"))
            .and(path("/repos/me/r/actions/runners"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"runners": []}))
                    .set_delay(delay),
            )
            .mount(&s)
            .await;
        let mut run = run_json(500, None);
        run["head_sha"] = json!("same-sha");
        mount_status(
            &s,
            "/repos/me/r/actions/runs",
            "in_progress",
            json!({"workflow_runs": [run]}),
        )
        .await;
        mount(&s, "/repos/me/r/actions/runs/500/jobs", json!({"jobs": [
            job(1, 500, "build", "in_progress", &["self-hosted"], Some((9, "box", "Default")), T0 - 60, Some(T0 - 30))]})).await;
        Mock::given(method("GET"))
            .and(path("/repos/me/r/commits/same-sha/pulls"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"number": 5, "state": "open"}])),
            )
            .expect(1)
            .mount(&s)
            .await;
        empty_runs_fallback(&s).await;
        servers.push(s);
    }
    let mut c = collector(vec![
        (pat_account(1, "a", &["me"]), client(&servers[0])),
        (pat_account(2, "b", &["me"]), client(&servers[1])),
    ]);
    let snap = c.tick(TickMode::Full).await;
    for (i, srv) in servers.iter().enumerate() {
        let r = &scope(&snap, i, "me").runners[0];
        assert_eq!(r.job.as_ref().unwrap().pr_tag, "PR#5");
        assert_eq!(
            count(srv, "/commits/same-sha/pulls").await,
            1,
            "伺服器 {i} 要被問過剛好一次"
        );
    }
}

#[tokio::test]
async fn malformed_success_body_marks_the_account_offline_and_keeps_the_last_view() {
    set_now(T0);
    let s = MockServer::start().await;
    let world = |bad: bool| {
        let s = &s;
        async move {
            s.reset().await;
            mount(s, "/user/repos", json!([repo("me/r", "User", T0, true)])).await;
            mount(
                s,
                "/repos/me/r/actions/runners",
                json!({"runners": [runner(1, "box", true, false, &["self-hosted"])]}),
            )
            .await;
            if bad {
                mount_status(
                    s,
                    "/repos/me/r/actions/runs",
                    "in_progress",
                    json!({"workflow_runs": "oops"}),
                )
                .await;
            }
            empty_runs_fallback(s).await;
        }
    };
    world(false).await;
    let mut c = collector(vec![(pat_account(1, "u", &["me"]), client(&s))]);
    let ok = c.tick(TickMode::Full).await;
    assert_eq!(ok.accounts[0].status, AccountStatus::Ok);

    world(true).await;
    set_now(T0 + 30);
    let bad = c.tick(TickMode::Incremental).await;
    assert_eq!(
        bad.accounts[0].status,
        AccountStatus::Offline { last_ok_at: T0 }
    );
    assert_eq!(
        runner_named(scope(&bad, 0, "me"), "box").state,
        RunnerState::Idle,
        "沿用上一份"
    );
}

#[tokio::test]
async fn repo_with_actions_disabled_does_not_take_the_account_down() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/repos",
        json!([
            repo("me/a", "User", T0, true),
            repo("me/b", "User", T0, true)
        ]),
    )
    .await;
    mount(
        &s,
        "/repos/me/a/actions/runners",
        json!({"runners": [runner(1, "box", true, false, &["self-hosted"])]}),
    )
    .await;
    mount(&s, "/repos/me/b/actions/runners", json!({"runners": []})).await;
    mount_code(&s, "/repos/me/b/actions/runs", 404).await; // Actions 停用
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(pat_account(1, "u", &["me"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(snap.accounts[0].status, AccountStatus::Ok);
    assert_eq!(scope(&snap, 0, "me").runners.len(), 1);
}

#[tokio::test]
async fn forbidden_repo_runner_roster_is_retried_only_when_sweeping() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(&s, "/user/repos", json!([repo("me/r", "User", T0, true)])).await;
    mount_code(&s, "/repos/me/r/actions/runners", 403).await;
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(pat_account(1, "u", &["me"]), client(&s))]);
    let t = c.tick(TickMode::Full).await;
    assert_eq!(t.accounts[0].status, AccountStatus::Ok);
    assert_eq!(count(&s, "/repos/me/r/actions/runners").await, 1);
    set_now(T0 + 30);
    c.tick(TickMode::Incremental).await;
    set_now(T0 + 60);
    c.tick(TickMode::Incremental).await;
    assert_eq!(
        count(&s, "/repos/me/r/actions/runners").await,
        1,
        "沒權限的名冊不每輪重打"
    );
    set_now(T0 + 601);
    c.tick(TickMode::Incremental).await;
    assert_eq!(
        count(&s, "/repos/me/r/actions/runners").await,
        2,
        "掃全部的窗口才重試"
    );
}

#[tokio::test]
async fn deselected_scope_state_is_dropped() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(&s, "/user/repos", json!([repo("me/r", "User", T0, true)])).await;
    mount(&s, "/repos/me/r/actions/runners", json!({"runners": []})).await;
    empty_runs_fallback(&s).await;
    let old = pr_runners_rules::ObservationState {
        last_observed_at: Some(T0 - 10),
        ..Default::default()
    };
    let states = BTreeMap::from([
        ("1/me".to_string(), old.clone()),
        ("1/gone".to_string(), old),
    ]);
    let mut c = Collector::new(
        vec![(pat_account(1, "u", &["me"]), client(&s))],
        states,
        2_500,
        now,
    );
    c.tick(TickMode::Full).await;
    let keys: Vec<_> = c.states().into_keys().collect();
    assert_eq!(keys, ["1/me"], "沒勾選的 scope 不留狀態");
}

// ───────── Collector 生命週期（Task 8 的 commands 用） ─────────

#[tokio::test]
async fn set_scopes_takes_effect_on_the_next_tick() {
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}},
        {"id": 2, "account": {"login": "B", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/r", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/2/repositories",
        json!({"repositories": [repo("B/r", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(1, "a1", true, false, &["self-hosted"])]}),
    )
    .await;
    mount(
        &s,
        "/orgs/B/actions/runners",
        json!({"runners": [runner(2, "b1", true, false, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&s, "A").await;
    empty_groups(&s, "B").await;
    // B 的 repo 上有一個正在跑的 job：要有 repo 清單才看得到它（只靠名冊看不到）
    mount_status(
        &s,
        "/repos/B/r/actions/runs",
        "in_progress",
        json!({"workflow_runs": [run_json(77, Some(2))]}),
    )
    .await;
    mount(&s, "/repos/B/r/actions/runs/77/jobs", json!({"jobs": [
        job(1, 77, "t", "in_progress", &["self-hosted"], Some((2, "b1", "Default")), T0 - 60, Some(T0 - 30))]})).await;
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "u", &["A"]), client(&s))]);
    let t1 = c.tick(TickMode::Full).await;
    assert_eq!(t1.accounts[0].scopes.len(), 1);

    // 新勾選 B：不用等 600 秒的冷窗口，下一個一般輪詢就有資料
    c.set_scopes(1, BTreeSet::from(["A".to_string(), "B".to_string()]));
    set_now(T0 + 30);
    let t2 = c.tick(TickMode::Incremental).await;
    assert_eq!(t2.accounts[0].scopes.len(), 2);
    let b1 = runner_named(scope(&t2, 0, "B"), "b1");
    assert_eq!(
        b1.state,
        RunnerState::Busy,
        "B 的 repo 清單要馬上探索到，才看得到它在跑的 job"
    );
    assert!(b1.job.is_some());
    assert_eq!(c.accounts()[0].selected_scopes.len(), 2);
}

#[tokio::test]
async fn add_remove_and_hold_accounts() {
    set_now(T0);
    let (good, other) = (MockServer::start().await, MockServer::start().await);
    for srv in [&good, &other] {
        mount(srv, "/user/repos", json!([repo("me/r", "User", T0, true)])).await;
        mount(
            srv,
            "/repos/me/r/actions/runners",
            json!({"runners": [runner(1, "box", true, false, &["self-hosted"])]}),
        )
        .await;
        empty_runs_fallback(srv).await;
    }
    let mut c = collector(vec![(pat_account(1, "a", &["me"]), client(&good))]);
    c.tick(TickMode::Full).await;

    c.add_account(pat_account(2, "b", &["me"]), client(&other));
    set_now(T0 + 5);
    let snap = c.tick(TickMode::Incremental).await;
    assert_eq!(
        snap.accounts
            .iter()
            .map(|a| a.account_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    // 續期失敗的帳號：不發請求、顯示對應狀態、沿用上一份內容；其他帳號照常
    let before = count(&other, "").await;
    c.hold(2, Some(Hold::NeedsReauth));
    set_now(T0 + 10);
    let held = c.tick(TickMode::Incremental).await;
    assert_eq!(held.accounts[1].status, AccountStatus::NeedsReauth);
    assert_eq!(held.accounts[0].status, AccountStatus::Ok);
    assert_eq!(count(&other, "").await, before, "hold 期間不發請求");
    c.hold(2, Some(Hold::Offline));
    let off = c.tick(TickMode::Incremental).await;
    assert!(matches!(
        off.accounts[1].status,
        AccountStatus::Offline { .. }
    ));
    assert!(!off.accounts[1].scopes.is_empty());
    c.hold(2, None);
    assert_eq!(
        c.tick(TickMode::Incremental).await.accounts[1].status,
        AccountStatus::Ok
    );

    c.remove_account(1);
    let only = c.tick(TickMode::Incremental).await;
    assert_eq!(only.accounts.len(), 1);
    assert!(!c.states().keys().any(|k| k.starts_with("1/")));
}

#[tokio::test]
async fn set_token_changes_the_token_used_by_following_requests() {
    set_now(T0);
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .and(wiremock::matchers::header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(10)
        .mount(&s)
        .await;
    let mut c = collector(vec![(pat_account(1, "u", &["u"]), client(&s))]);
    assert_eq!(
        c.tick(TickMode::Full).await.accounts[0].status,
        AccountStatus::NeedsReauth
    );
    c.set_token(1, "fresh".into());
    set_now(T0 + 200);
    assert_eq!(
        c.tick(TickMode::Full).await.accounts[0].status,
        AccountStatus::Ok
    );
}

// ── 預估剩餘時間 ──

fn done_job(id: u64, run_id: u64, name: &str, runner: &str, secs: i64, conclusion: &str) -> Value {
    let start = T0 - 100_000 + id as i64 * 10;
    json!({"id": id, "run_id": run_id, "name": name, "status": "completed", "conclusion": conclusion,
           "labels": ["self-hosted"], "runner_id": 9, "runner_name": runner, "runner_group_name": "Default",
           "created_at": iso(start), "started_at": iso(start), "completed_at": iso(start + secs),
           "workflow_name": "ci", "run_attempt": 1, "html_url": ""})
}

/// 一個 org、一台 runner 正在跑 workflow 77 的 `build`；workflow 77 最近成功過 `history.len()` 次。
async fn eta_server(history: &[(u64, i64)]) -> MockServer {
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"total_count": 1, "installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(9, "builder-1", true, true, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&s, "A").await;
    let mut run = run_json(500, Some(3));
    run["workflow_id"] = json!(77);
    mount_status(
        &s,
        "/repos/A/ra/actions/runs",
        "in_progress",
        json!({"workflow_runs": [run]}),
    )
    .await;
    mount(&s, "/repos/A/ra/actions/runs/500/jobs", json!({"jobs": [
        job(1, 500, "build", "in_progress", &["self-hosted"], Some((9, "builder-1", "Default")), T0 - 700, Some(T0 - 600))]})).await;
    empty_runs_fallback(&s).await;
    let ids: Vec<Value> = history.iter().map(|(id, _)| json!({"id": id})).collect();
    Mock::given(method("GET"))
        .and(path("/repos/A/ra/actions/workflows/77/runs"))
        .and(query_param("status", "success"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": ids})))
        .mount(&s)
        .await;
    for (i, (id, secs)) in history.iter().enumerate() {
        mount(
            &s,
            &format!("/repos/A/ra/actions/runs/{id}/jobs"),
            json!({"jobs": [
            done_job(10 + i as u64, *id, "build", "builder-1", *secs, "success"),
            // 被略過與失敗的 job 不算進樣本
            done_job(100 + i as u64, *id, "build", "builder-1", 5, "skipped"),
            done_job(200 + i as u64, *id, "lint", "builder-1", 60, "failure")]}),
        )
        .await;
    }
    s
}

fn running_job(snap: &Snapshot) -> &JobView {
    runner_named(scope(snap, 0, "A"), "builder-1")
        .job
        .as_ref()
        .unwrap()
}

#[tokio::test]
async fn running_job_gets_typical_duration_from_recent_successful_runs() {
    set_now(T0);
    let s = eta_server(&[(401, 1_000), (402, 1_200), (403, 1_100)]).await;
    let mut c = collector(vec![(app_account(1, "me", &["A"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(
        running_job(&snap).typical_secs,
        Some(1_100),
        "三次成功的中位數；略過與失敗的不算"
    );

    // 下一輪：已完成的執行不再重抓 job 清單，執行清單也還沒到重新確認的時間
    let before = count(&s, "/actions/runs/40").await;
    let list_before = count(&s, "/actions/workflows/77/runs").await;
    set_now(T0 + 30);
    let snap = c.tick(TickMode::Incremental).await;
    assert_eq!(running_job(&snap).typical_secs, Some(1_100));
    assert_eq!(
        count(&s, "/actions/runs/40").await,
        before,
        "完成的執行只抓一次"
    );
    assert_eq!(
        count(&s, "/actions/workflows/77/runs").await,
        list_before,
        "15 分鐘內不重新確認"
    );
}

#[tokio::test]
async fn too_few_successful_runs_means_no_estimate() {
    set_now(T0);
    let s = eta_server(&[(401, 1_000), (402, 1_200)]).await;
    let mut c = collector(vec![(app_account(1, "me", &["A"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(running_job(&snap).typical_secs, None);
}

#[tokio::test]
async fn runs_without_workflow_id_never_query_history() {
    // 既有的測試替身沒有 workflow_id：不能多打任何歷史查詢
    set_now(T0);
    let s = eta_server(&[(401, 1_000), (402, 1_200), (403, 1_100)]).await;
    s.reset().await;
    mount(
        &s,
        "/user/installations",
        json!({"total_count": 1, "installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}}]}),
    )
    .await;
    mount(
        &s,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    mount(
        &s,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(9, "builder-1", true, true, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&s, "A").await;
    mount_status(
        &s,
        "/repos/A/ra/actions/runs",
        "in_progress",
        json!({"workflow_runs": [run_json(500, Some(3))]}),
    )
    .await;
    mount(&s, "/repos/A/ra/actions/runs/500/jobs", json!({"jobs": [
        job(1, 500, "build", "in_progress", &["self-hosted"], Some((9, "builder-1", "Default")), T0 - 700, Some(T0 - 600))]})).await;
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "me", &["A"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    assert_eq!(running_job(&snap).typical_secs, None);
    assert_eq!(count(&s, "/actions/workflows/").await, 0);
}

#[tokio::test]
async fn history_fetch_budget_is_shared_across_scopes_in_one_tick() {
    // 兩個 org 各有一個正在跑的 workflow，各有 8 次成功執行要補：一輪總共最多補 10 次
    set_now(T0);
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"total_count": 2, "installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}},
        {"id": 2, "account": {"login": "B", "type": "Organization"}}]}),
    )
    .await;
    for (inst, org) in [(1, "A"), (2, "B")] {
        let repo_name = format!("{org}/r");
        mount(
            &s,
            &format!("/user/installations/{inst}/repositories"),
            json!({"repositories": [repo(&repo_name, "Organization", T0, true)]}),
        )
        .await;
        mount(
            &s,
            &format!("/orgs/{org}/actions/runners"),
            json!({"runners": [runner(9, "builder-1", true, true, &["self-hosted"])]}),
        )
        .await;
        empty_groups(&s, org).await;
        let mut run = run_json(500, Some(3));
        run["workflow_id"] = json!(77);
        mount_status(
            &s,
            &format!("/repos/{repo_name}/actions/runs"),
            "in_progress",
            json!({"workflow_runs": [run]}),
        )
        .await;
        mount(&s, &format!("/repos/{repo_name}/actions/runs/500/jobs"), json!({"jobs": [
            job(1, 500, "build", "in_progress", &["self-hosted"], Some((9, "builder-1", "Default")), T0 - 700, Some(T0 - 600))]})).await;
        let ids: Vec<Value> = (401..409).map(|id| json!({"id": id})).collect();
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/{repo_name}/actions/workflows/77/runs"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": ids})))
            .mount(&s)
            .await;
        for id in 401..409u64 {
            mount(
                &s,
                &format!("/repos/{repo_name}/actions/runs/{id}/jobs"),
                json!({"jobs": [
                done_job(id, id, "build", "builder-1", 600, "success")]}),
            )
            .await;
        }
    }
    empty_runs_fallback(&s).await;
    let mut c = collector(vec![(app_account(1, "me", &["A", "B"]), client(&s))]);
    c.tick(TickMode::Full).await;
    assert_eq!(
        count(&s, "/actions/runs/40").await,
        10,
        "兩個範圍共用一輪 10 次的額度"
    );
    // 下一輪補完剩下的 6 次
    set_now(T0 + 30);
    c.tick(TickMode::Incremental).await;
    assert_eq!(count(&s, "/actions/runs/40").await, 16);
}

// ── runner 執行歷史 ──

/// 一個 org、一台 runner；`running` 為 true 時它正在跑 job 1（run 500）。
async fn history_server(running: bool) -> MockServer {
    let s = MockServer::start().await;
    mount(
        &s,
        "/user/installations",
        json!({"total_count": 1, "installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}}]}),
    )
    .await;
    mount(&s, "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true), repo("A/old", "Organization", T0 - 30 * 86_400, true)]})).await;
    mount(
        &s,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(9, "builder-1", true, running, &["self-hosted"])]}),
    )
    .await;
    empty_groups(&s, "A").await;
    if running {
        mount_status(
            &s,
            "/repos/A/ra/actions/runs",
            "in_progress",
            json!({"workflow_runs": [run_json(500, Some(3))]}),
        )
        .await;
        mount(&s, "/repos/A/ra/actions/runs/500/jobs", json!({"jobs": [
            job(1, 500, "build", "in_progress", &["self-hosted"], Some((9, "builder-1", "Default")), T0 - 700, Some(T0 - 600))]})).await;
    }
    empty_runs_fallback(&s).await;
    s
}

#[tokio::test]
async fn runner_history_records_running_jobs_and_their_final_result() {
    set_now(T0);
    let s = history_server(true).await;
    let mut c = collector(vec![(app_account(1, "me", &["A"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    let key = runner_named(scope(&snap, 0, "A"), "builder-1").key.clone();
    let recs = c.runlog().records(&key);
    assert_eq!(recs.len(), 1);
    assert_eq!(
        (recs[0].job_id, recs[0].outcome, recs[0].source),
        (1, JobOutcome::Running, RecordSource::Observed)
    );
    assert_eq!(recs[0].started_at, Some(T0 - 600));
    assert_eq!(count(&s, "/actions/jobs/1").await, 0, "還在跑的不查結果");

    // job 跑完、從畫面消失：查一次最終結果
    s.reset().await;
    let s2 = &s;
    mount(
        s2,
        "/user/installations",
        json!({"total_count": 1, "installations": [
        {"id": 1, "account": {"login": "A", "type": "Organization"}}]}),
    )
    .await;
    mount(
        s2,
        "/user/installations/1/repositories",
        json!({"repositories": [repo("A/ra", "Organization", T0, true)]}),
    )
    .await;
    mount(
        s2,
        "/orgs/A/actions/runners",
        json!({"runners": [runner(9, "builder-1", true, false, &["self-hosted"])]}),
    )
    .await;
    empty_groups(s2, "A").await;
    empty_runs_fallback(s2).await;
    mount(s2, "/repos/A/ra/actions/jobs/1", json!({"id": 1, "run_id": 500, "name": "build", "status": "completed",
        "conclusion": "failure", "labels": ["self-hosted"], "runner_id": 9, "runner_name": "builder-1",
        "runner_group_name": "Default", "created_at": iso(T0 - 700), "started_at": iso(T0 - 600),
        "completed_at": iso(T0 - 100), "workflow_name": "ci", "html_url": ""})).await;
    set_now(T0 + 200);
    c.tick(TickMode::Incremental).await;
    let recs = c.runlog().records(&key);
    assert_eq!(
        (recs[0].outcome, recs[0].completed_at),
        (JobOutcome::Failure, Some(T0 - 100))
    );
    assert_eq!(count(&s, "/actions/jobs/1").await, 1);
    // 已有結果：之後不再查
    set_now(T0 + 600);
    c.tick(TickMode::Incremental).await;
    assert_eq!(count(&s, "/actions/jobs/1").await, 1);
}

#[tokio::test]
async fn backfill_picks_jobs_that_ran_on_this_runner_from_recently_pushed_repos() {
    set_now(T0);
    let s = history_server(false).await;
    Mock::given(method("GET"))
        .and(path("/repos/A/ra/actions/runs"))
        .and(query_param("status", "completed"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"workflow_runs": [run_json(600, Some(4)), run_json(601, None)]}),
            ),
        )
        .mount(&s)
        .await;
    let done = |id: u64, run: u64, runner: &str, conclusion: &str| -> Value {
        // 名冊上的 builder-1 是 runner 9；同名的別台機器 id 不同
        let rid = match (id, runner) {
            (14, _) => 77,
            (_, "builder-1") => 9,
            _ => 50,
        };
        json!({"id": id, "run_id": run, "name": format!("job-{id}"), "status": "completed", "conclusion": conclusion,
               "labels": [], "runner_id": rid, "runner_name": runner, "runner_group_name": "Default",
               "created_at": iso(T0 - 5_000), "started_at": iso(T0 - 4_000 + id as i64), "completed_at": iso(T0 - 3_000),
               "workflow_name": "ci", "html_url": ""})
    };
    mount(
        &s,
        "/repos/A/ra/actions/runs/600/jobs",
        json!({"jobs": [done(11, 600, "builder-1", "success"), done(12, 600, "other", "success")]}),
    )
    .await;
    mount(
        &s,
        "/repos/A/ra/actions/runs/601/jobs",
        // job 14 跑在另一台同名的 builder-1（id 77）：不能算進名冊上這台（id 9）
        json!({"jobs": [done(13, 601, "builder-1", "cancelled"), done(14, 601, "builder-1", "success")]}),
    )
    .await;

    let mut c = collector(vec![(app_account(1, "me", &["A"]), client(&s))]);
    let snap = c.tick(TickMode::Full).await;
    let key = runner_named(scope(&snap, 0, "A"), "builder-1").key.clone();
    let old_before = count(&s, "/repos/A/old/").await;
    let report = c.backfill(1, "A", "builder-1", &key).await;
    assert_eq!(
        (
            report.repos_checked,
            report.runs_checked,
            report.added,
            report.partial
        ),
        (1, 2, 2, false),
        "只查最近 7 天有推送的 repo；只收這台 runner 的 job"
    );
    assert_eq!(
        count(&s, "/repos/A/old/").await,
        old_before,
        "30 天沒推送的 repo 補查時不查"
    );
    let recs = c.runlog().records(&key);
    let got: Vec<(u64, JobOutcome, RecordSource)> = recs
        .iter()
        .map(|r| (r.job_id, r.outcome, r.source))
        .collect();
    assert_eq!(
        got,
        vec![
            (13, JobOutcome::Cancelled, RecordSource::Backfill),
            (11, JobOutcome::Success, RecordSource::Backfill)
        ]
    );
    assert_eq!(recs[1].pr_tag, "PR#4");
    // 再補一次：不會重複加入
    assert_eq!(c.backfill(1, "A", "builder-1", &key).await.added, 0);
}
