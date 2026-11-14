//! PR 標籤：標籤規則與 commit → PR 查詢的快取。

use pr_runners_core::GitHubClient;
use pr_runners_core::api::RunInfo;
use pr_runners_core::pr_tag::{PrTagCache, pr_tag};
use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// 同一個 repo 的 run 是並行查的：一次 push 觸發好幾個 workflow（sha 都一樣）時，
/// 同時來的查詢要共用同一個請求，不能每個 run 各打一次（每次都計費）。
#[tokio::test]
async fn concurrent_pr_lookups_for_the_same_sha_send_one_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits/same/pulls"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"number": 5, "state": "open"}]))
                // 讓第一個請求還在途時，其他查詢已經到了
                .set_delay(std::time::Duration::from_millis(100)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (c, cache) = (client(&server), PrTagCache::default());
    let runs = [
        run("push", None, Some("main"), "same"),
        run("push", None, Some("main"), "same"),
        run("push", None, Some("main"), "same"),
    ];
    let tags = futures_util::future::join_all(runs.iter().map(|r| pr_tag(&c, &cache, r))).await;
    for t in tags {
        assert_eq!(t.unwrap(), "push:main←#5");
    }
}
