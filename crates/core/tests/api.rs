use pr_runners_core::api::*;
use pr_runners_core::{ApiError, GitHubClient};
use pr_runners_rules::{GroupAccess, JobState};
use serde_json::json;
use std::collections::BTreeSet;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("fixtures/", $name))
    };
}

fn client(server: &MockServer) -> GitHubClient {
    GitHubClient::new(Url::parse(&server.uri()).unwrap(), "tok".into())
}

async fn serve(server: &MockServer, p: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json"),
        )
        .mount(server)
        .await;
}

async fn serve_org_groups(server: &MockServer) {
    serve(
        server,
        "/orgs/Acme/actions/runner-groups",
        fixture!("runner_groups.json"),
    )
    .await;
    for (g, runners) in [
        (1, fixture!("group_1_runners.json")),
        (3, fixture!("group_3_runners.json")),
        (4, fixture!("group_4_runners.json")),
        (7, fixture!("group_7_runners.json")),
    ] {
        serve(
            server,
            &format!("/orgs/Acme/actions/runner-groups/{g}/runners"),
            runners,
        )
        .await;
    }
    for (g, repos) in [
        (3, fixture!("group_3_repositories.json")),
        (4, fixture!("group_4_repositories.json")),
        (7, fixture!("group_7_repositories.json")),
    ] {
        serve(
            server,
            &format!("/orgs/Acme/actions/runner-groups/{g}/repositories"),
            repos,
        )
        .await;
    }
}

#[tokio::test]
async fn maps_org_runners() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/orgs/Acme/actions/runners",
        fixture!("org_runners.json"),
    )
    .await;
    let rs = org_runners(&client(&server), "Acme").await.unwrap();
    assert_eq!(rs.len(), 5);
    let mac1 = rs
        .iter()
        .find(|r| r.name == "mac-mini-1")
        .unwrap();
    assert_eq!(mac1.id, 22);
    assert!(mac1.online && !mac1.busy);
    assert!(mac1.labels.iter().any(|l| l == "macOS"));
    assert_eq!(mac1.os, "macOS");
}

#[tokio::test]
async fn offline_and_busy_flags_are_mapped() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/repos/o/r/actions/runners",
        &json!({"total_count": 2, "runners": [
            {"id": 1, "name": "a", "os": "Linux", "status": "offline", "busy": false, "labels": [{"name": "self-hosted"}]},
            {"id": 2, "name": "b", "os": "Linux", "status": "online", "busy": true, "labels": []},
        ]})
        .to_string(),
    )
    .await;
    let rs = repo_runners(&client(&server), "o/r").await.unwrap();
    assert!(!rs[0].online && !rs[0].busy);
    assert!(rs[1].online && rs[1].busy);
}

#[tokio::test]
async fn group_selected_repos_become_selected_access() {
    let server = MockServer::start().await;
    serve_org_groups(&server).await;
    let groups = org_runner_groups(&client(&server), "Acme")
        .await
        .unwrap();
    let g3 = groups.iter().find(|g| g.id == 3).unwrap();
    match &g3.access {
        GroupAccess::Selected { repos, public_ok } => {
            assert!(repos.contains("Acme/acme-web"));
            assert!(!public_ok);
        }
        other => panic!("{other:?}"),
    }
    let g1 = groups.iter().find(|g| g.id == 1).unwrap();
    assert_eq!(g1.access, GroupAccess::All { public_ok: false });
    assert!(!g3.restricted_to_workflows);
}

#[tokio::test]
async fn group_membership_comes_from_group_runners() {
    let server = MockServer::start().await;
    serve_org_groups(&server).await;
    let groups = org_runner_groups(&client(&server), "Acme")
        .await
        .unwrap();
    // org_runners 的 id：linux-box-1=6、vps-1=29、vps-2=31、mac1=22
    let ids = |g: u64| {
        groups
            .iter()
            .find(|x| x.id == g)
            .unwrap()
            .runner_ids
            .clone()
    };
    assert_eq!(ids(3), BTreeSet::from([6, 29, 31]));
    assert_eq!(ids(1), BTreeSet::from([22]));
    assert!(ids(4).is_empty());
}

#[tokio::test]
async fn group_without_permission_degrades_to_unknown() {
    let server = MockServer::start().await;
    serve(&server, "/orgs/o/actions/runner-groups", &json!({"total_count": 1, "runner_groups": [
        {"id": 3, "name": "g", "visibility": "selected", "allows_public_repositories": false, "restricted_to_workflows": false}]}).to_string()).await;
    serve(
        &server,
        "/orgs/o/actions/runner-groups/3/runners",
        &json!({"total_count": 0, "runners": []}).to_string(),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/orgs/o/actions/runner-groups/3/repositories"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "nope"})))
        .mount(&server)
        .await;
    let groups = org_runner_groups(&client(&server), "o").await.unwrap();
    assert_eq!(groups[0].access, GroupAccess::Unknown);
}

#[tokio::test]
async fn group_rate_limit_is_propagated_not_swallowed() {
    let server = MockServer::start().await;
    serve(&server, "/orgs/o/actions/runner-groups", &json!({"total_count": 1, "runner_groups": [
        {"id": 3, "name": "g", "visibility": "all", "allows_public_repositories": true, "restricted_to_workflows": false}]}).to_string()).await;
    Mock::given(method("GET"))
        .and(path("/orgs/o/actions/runner-groups/3/runners"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "60"))
        .mount(&server)
        .await;
    let r = org_runner_groups(&client(&server), "o").await;
    assert!(matches!(r, Err(ApiError::RateLimited { .. })), "{r:?}");
}

#[tokio::test]
async fn private_and_restricted_groups_map() {
    let server = MockServer::start().await;
    serve(&server, "/orgs/o/actions/runner-groups", &json!({"total_count": 1, "runner_groups": [
        {"id": 5, "name": "p", "visibility": "private", "allows_public_repositories": false, "restricted_to_workflows": true}]}).to_string()).await;
    serve(
        &server,
        "/orgs/o/actions/runner-groups/5/runners",
        &json!({"total_count": 0, "runners": []}).to_string(),
    )
    .await;
    let g = &org_runner_groups(&client(&server), "o").await.unwrap()[0];
    assert_eq!(g.access, GroupAccess::Private);
    assert!(
        g.restricted_to_workflows,
        "restricted 另存，不改動已知的 repo／public 限制"
    );
}

#[tokio::test]
async fn run_jobs_keeps_only_active_states() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/repos/Acme/pr-runners/actions/runs/1001/jobs",
        fixture!("run_jobs.json"),
    )
    .await;
    let run = RunInfo {
        id: 1001,
        repo: "Acme/pr-runners".into(),
        workflow_id: None,
        workflow_name: "ci".into(),
        event: "pull_request".into(),
        head_branch: Some("feat/x".into()),
        head_sha: "abc".into(),
        pr_number: Some(42),
        html_url: String::new(),
    };
    let jobs = run_jobs(&client(&server), &run, true).await.unwrap();
    let states: Vec<_> = jobs
        .iter()
        .map(|j| (j.job_id, j.facts.state.clone()))
        .collect();
    assert_eq!(
        states,
        vec![
            (9002, JobState::Queued),
            (9003, JobState::Waiting),
            (9004, JobState::InProgress)
        ]
    );
    assert_eq!(jobs[0].facts.key, "1001/9002");
    assert!(jobs[0].facts.repo_public);
    assert_eq!(jobs[0].facts.labels, ["self-hosted", "macOS", "ARM64"]);
    // 尚未指派：null／0／空字串都正規化成 None
    assert_eq!(jobs[0].facts.runner_id, None);
    assert_eq!(jobs[1].facts.runner_id, None);
    assert_eq!(jobs[1].facts.runner_name, None);
    assert_eq!(jobs[1].facts.runner_group_name, None);
    assert_eq!(jobs[2].facts.runner_id, Some(22));
    assert_eq!(
        jobs[2].facts.runner_name.as_deref(),
        Some("mac-mini-1")
    );
    assert_eq!(jobs[2].facts.runner_group_name.as_deref(), Some("Default"));
    assert_eq!(jobs[2].job_name, "test-on-mac1");
    assert_eq!(jobs[2].workflow_name, "ci");
    assert_eq!(jobs[2].run_attempt, 1);
    // ISO 8601 → Unix 秒
    assert_eq!(
        jobs[2].facts.created_at,
        parse_ts("2026-11-02T11:02:48Z").unwrap()
    );
    assert!(jobs[2].facts.started_at.is_some());
    assert_eq!(jobs[0].facts.started_at, None);
}

#[tokio::test]
async fn active_runs_merges_three_statuses_and_dedupes_by_run_id() {
    let server = MockServer::start().await;
    for (status, body) in [
        ("in_progress", fixture!("runs_in_progress.json")),
        ("queued", fixture!("runs_queued.json")),
        ("pending", fixture!("runs_pending.json")),
    ] {
        Mock::given(method("GET"))
            .and(path("/repos/Acme/pr-runners/actions/runs"))
            .and(query_param("status", status))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body.as_bytes().to_vec(), "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let runs = active_runs(&client(&server), "Acme/pr-runners")
        .await
        .unwrap();
    let ids: Vec<u64> = runs.iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        [1001, 1002, 1003, 1004],
        "1002 在三份清單中出現多次，只留一筆"
    );
    let r1001 = &runs[0];
    assert_eq!(r1001.pr_number, Some(42));
    assert_eq!(r1001.event, "pull_request");
    assert_eq!(r1001.head_branch.as_deref(), Some("feat/x"));
    assert_eq!(r1001.repo, "Acme/pr-runners");
    assert_eq!(runs[1].pr_number, None);
}

fn run(event: &str, pr: Option<u64>, branch: Option<&str>, sha: &str) -> RunInfo {
    RunInfo {
        id: 1,
        repo: "o/r".into(),
        workflow_id: None,
        workflow_name: "ci".into(),
        event: event.into(),
        head_branch: branch.map(str::to_string),
        head_sha: sha.into(),
        pr_number: pr,
        html_url: String::new(),
    }
}

#[tokio::test]
async fn pr_tag_rules() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/repos/o/r/commits/s-push/pulls",
        &json!([{"number": 9, "state": "closed"}]).to_string(),
    )
    .await;
    serve(
        &server,
        "/repos/o/r/commits/s-pr/pulls",
        &json!([{"number": 17, "state": "closed"}]).to_string(),
    )
    .await;
    serve(
        &server,
        "/repos/o/r/commits/s-open/pulls",
        &json!([{"number": 7, "state": "open"}]).to_string(),
    )
    .await;
    serve(&server, "/repos/o/r/commits/s-none/pulls", "[]").await;
    let (c, cache) = (client(&server), PrTagCache::default());

    // run 本身帶 PR 號：不打任何請求
    assert_eq!(
        pr_tag(
            &c,
            &cache,
            &run("pull_request", Some(42), Some("x"), "unmocked")
        )
        .await
        .unwrap(),
        "PR#42"
    );
    // push ＋ 反查到來源 PR
    assert_eq!(
        pr_tag(&c, &cache, &run("push", None, Some("main"), "s-push"))
            .await
            .unwrap(),
        "push:main←#9"
    );
    // push 反查不到
    assert_eq!(
        pr_tag(&c, &cache, &run("push", None, Some("main"), "s-none"))
            .await
            .unwrap(),
        "push:main"
    );
    // 其他事件：closed → PR#N(closed)；open → PR#N
    assert_eq!(
        pr_tag(&c, &cache, &run("pull_request", None, Some("f"), "s-pr"))
            .await
            .unwrap(),
        "PR#17(closed)"
    );
    assert_eq!(
        pr_tag(&c, &cache, &run("pull_request", None, Some("f"), "s-open"))
            .await
            .unwrap(),
        "PR#7"
    );
    // 反查不到 → {event}:{branch}；沒有 branch 就只有 event
    assert_eq!(
        pr_tag(&c, &cache, &run("schedule", None, Some("main"), "s-none"))
            .await
            .unwrap(),
        "schedule:main"
    );
    assert_eq!(
        pr_tag(&c, &cache, &run("workflow_dispatch", None, None, "s-none"))
            .await
            .unwrap(),
        "workflow_dispatch"
    );
}

#[tokio::test]
async fn pr_tag_is_cached_per_sha() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits/same/pulls"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([{"number": 5, "state": "open"}])),
        )
        .expect(1) // 同一個 sha 兩次，只打一次
        .mount(&server)
        .await;
    let (c, cache) = (client(&server), PrTagCache::default());
    assert_eq!(
        pr_tag(&c, &cache, &run("pull_request", None, Some("f"), "same"))
            .await
            .unwrap(),
        "PR#5"
    );
    // 同 SHA 的 push run：共用查詢結果，但標籤依自己的 event 組成
    assert_eq!(
        pr_tag(&c, &cache, &run("push", None, Some("main"), "same"))
            .await
            .unwrap(),
        "push:main←#5"
    );
}

#[tokio::test]
async fn installations_and_repos() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/user/installations",
        &json!({"total_count": 2, "installations": [
        {"id": 11, "account": {"login": "Acme", "type": "Organization"}},
        {"id": 12, "account": {"login": "jdoe", "type": "User"}}]})
        .to_string(),
    )
    .await;
    serve(
        &server,
        "/user/installations/11/repositories",
        &json!({"total_count": 1, "repositories": [
        {"full_name": "Acme/pr-runners", "private": true, "pushed_at": "2026-11-02T11:00:00Z",
         "owner": {"login": "Acme", "type": "Organization"}}]})
        .to_string(),
    )
    .await;
    let c = client(&server);
    let ins = list_installations(&c).await.unwrap();
    assert_eq!(
        ins[0],
        Installation {
            id: 11,
            account_login: "Acme".into(),
            account_kind: ScopeKind::Org
        }
    );
    assert_eq!(ins[1].account_kind, ScopeKind::User);
    let repos = list_installation_repos(&c, 11).await.unwrap();
    assert_eq!(repos[0].full_name, "Acme/pr-runners");
    assert!(repos[0].private);
    assert_eq!(
        repos[0].pushed_at,
        parse_ts("2026-11-02T11:00:00Z").unwrap()
    );
    assert_eq!(repos[0].owner_kind, ScopeKind::Org);
}

#[tokio::test]
async fn pat_account_scopes_come_from_user_repos_owners() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/user",
        &json!({"id": 99, "login": "jdoe"}).to_string(),
    )
    .await;
    serve(&server, "/user/repos", &json!([
        {"full_name": "jdoe/a", "private": false, "pushed_at": null, "owner": {"login": "jdoe", "type": "User"}},
        {"full_name": "Acme/b", "private": true, "pushed_at": "2026-01-01T00:00:00Z", "owner": {"login": "Acme", "type": "Organization"}}
    ]).to_string()).await;
    let c = client(&server);
    assert_eq!(
        current_user(&c).await.unwrap(),
        (99, "jdoe".to_string())
    );
    let repos = list_user_repos(&c).await.unwrap();
    assert_eq!(repos[0].owner_kind, ScopeKind::User);
    assert_eq!(repos[0].pushed_at, 0, "pushed_at 為 null 時是 0");
    assert_eq!(repos[1].owner_login, "Acme");
    assert_eq!(repos[1].owner_kind, ScopeKind::Org);
}

#[test]
fn parse_ts_rejects_garbage() {
    assert!(parse_ts("yesterday").is_err());
    assert_eq!(parse_ts("1970-01-01T00:01:00Z").unwrap(), 60);
    assert_eq!(parse_ts("1970-01-01T01:00:00+01:00").unwrap(), 0);
}
