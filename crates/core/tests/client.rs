use pr_runners_core::{ApiError, GitHubClient};
use serde::Deserialize;
use serde_json::json;
use url::Url;
use wiremock::matchers::{header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> GitHubClient {
    GitHubClient::new(Url::parse(&server.uri()).unwrap(), "tok".into())
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[derive(Debug, Deserialize, PartialEq)]
struct Item {
    n: u32,
}

#[tokio::test]
async fn etag_304_returns_cached_and_not_counted() {
    let server = MockServer::start().await;
    // 第二次請求：必須帶 If-None-Match: "a"，回 304
    Mock::given(method("GET"))
        .and(path("/thing"))
        .and(header("if-none-match", "\"a\""))
        .and(header_regex("user-agent", "^pr-runners/"))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;
    // 第一次：200 ＋ ETag
    Mock::given(method("GET"))
        .and(path("/thing"))
        .and(header_regex("user-agent", "^pr-runners/"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"a\"")
                .set_body_json(json!({"n": 7})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let c = client(&server);
    assert_eq!(c.get_json::<Item>("/thing").await.unwrap(), Item { n: 7 });
    assert_eq!(c.rate().used_this_hour, 1);
    assert_eq!(c.get_json::<Item>("/thing").await.unwrap(), Item { n: 7 });
    assert_eq!(c.rate().used_this_hour, 1, "經授權的 304 不計費");
}

#[tokio::test]
async fn records_rate_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-RateLimit-Limit", "5000")
                .insert_header("X-RateLimit-Remaining", "4321")
                .insert_header("X-RateLimit-Reset", "1900000000")
                .set_body_json(json!({"n": 1})),
        )
        .mount(&server)
        .await;
    let c = client(&server);
    c.get_json::<Item>("/x").await.unwrap();
    let r = c.rate();
    assert_eq!(
        (r.limit, r.remaining, r.reset_at),
        (5000, 4321, 1_900_000_000)
    );
}

#[tokio::test]
async fn follows_link_next_across_pages() {
    // Review Focus 1：超過 100 筆要全部撈到
    let server = MockServer::start().await;
    let page = |from: u32, count: u32| {
        (from..from + count)
            .map(|n| json!({"n": n}))
            .collect::<Vec<_>>()
    };
    let next = |p: u32| {
        format!(
            "<{}/items?per_page=100&page={p}>; rel=\"next\"",
            server.uri()
        )
    };
    for (p, count, link) in [(2, 100, Some(next(3))), (3, 5, None)] {
        let mut t =
            ResponseTemplate::new(200).set_body_json(json!({"items": page((p - 1) * 100, count)}));
        if let Some(l) = link {
            t = t.insert_header("Link", l.as_str());
        }
        Mock::given(method("GET"))
            .and(path("/items"))
            .and(query_param("page", p.to_string()))
            .respond_with(t)
            .expect(1)
            .mount(&server)
            .await;
    }
    // 第一頁的網址沒有 page 參數
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(wiremock::matchers::query_param_is_missing("page"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", next(2).as_str())
                .set_body_json(json!({"items": page(0, 100)})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let all: Vec<Item> = c
        .get_paginated("/items?per_page=100", Some("items"))
        .await
        .unwrap();
    assert_eq!(all.len(), 205);
    assert_eq!(all[0], Item { n: 0 });
    assert_eq!(all[204], Item { n: 204 });
}

#[tokio::test]
async fn bare_array_pagination_without_items_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"n": 1}, {"n": 2}])))
        .mount(&server)
        .await;
    let all: Vec<Item> = client(&server).get_paginated("/x", None).await.unwrap();
    assert_eq!(all, vec![Item { n: 1 }, Item { n: 2 }]);
}

#[tokio::test]
async fn refuses_link_pointing_to_another_origin() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", "<https://evil.example/x?page=2>; rel=\"next\"")
                .set_body_json(json!([{"n": 1}])),
        )
        .mount(&server)
        .await;
    let r: Result<Vec<Item>, _> = client(&server).get_paginated("/x", None).await;
    assert!(matches!(r, Err(ApiError::Decode(_))), "{r:?}");
}

async fn one_status(template: ResponseTemplate) -> (Result<Item, ApiError>, i64) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(template)
        .mount(&server)
        .await;
    let before = now();
    (client(&server).get_json::<Item>("/x").await, before)
}

#[tokio::test]
async fn classifies_errors() {
    let (r, _) = one_status(ResponseTemplate::new(401)).await;
    assert_eq!(r, Err(ApiError::Unauthorized));

    let (r, _) = one_status(ResponseTemplate::new(404)).await;
    assert_eq!(r, Err(ApiError::NotFound));

    // primary：remaining 0 → 等到 X-RateLimit-Reset
    let (r, _) = one_status(
        ResponseTemplate::new(403)
            .insert_header("X-RateLimit-Remaining", "0")
            .insert_header("X-RateLimit-Reset", "4000000000"),
    )
    .await;
    assert_eq!(
        r,
        Err(ApiError::RateLimited {
            reset_at: 4_000_000_000
        })
    );

    // 429 ＋ Retry-After
    let (r, before) =
        one_status(ResponseTemplate::new(429).insert_header("Retry-After", "120")).await;
    let Err(ApiError::RateLimited { reset_at }) = r else {
        panic!("{r:?}")
    };
    assert!(
        (before + 120..=before + 122).contains(&reset_at),
        "{reset_at} vs {before}"
    );

    // secondary 的 403 ＋ Retry-After 也是限流，不是權限不足
    let (r, before) =
        one_status(ResponseTemplate::new(403).insert_header("Retry-After", "30")).await;
    let Err(ApiError::RateLimited { reset_at }) = r else {
        panic!("{r:?}")
    };
    assert!((before + 30..=before + 32).contains(&reset_at));

    // primary reset 與 Retry-After 並存：取較晚者
    let (r, _) = one_status(
        ResponseTemplate::new(403)
            .insert_header("X-RateLimit-Remaining", "0")
            .insert_header("X-RateLimit-Reset", "4000000000")
            .insert_header("Retry-After", "30"),
    )
    .await;
    assert_eq!(
        r,
        Err(ApiError::RateLimited {
            reset_at: 4_000_000_000
        })
    );

    // secondary 沒有任何等待標頭：至少 60 秒
    let (r, before) = one_status(
        ResponseTemplate::new(403)
            .set_body_json(json!({"message": "You have exceeded a secondary rate limit."})),
    )
    .await;
    let Err(ApiError::RateLimited { reset_at }) = r else {
        panic!("{r:?}")
    };
    assert!(reset_at >= before + 60, "{reset_at} vs {before}");

    // 一般權限不足
    let (r, _) = one_status(
        ResponseTemplate::new(403)
            .insert_header("X-RateLimit-Remaining", "4000")
            .set_body_json(json!({"message": "Resource not accessible by integration"})),
    )
    .await;
    assert_eq!(
        r,
        Err(ApiError::Forbidden {
            message: "Resource not accessible by integration".into()
        })
    );

    // 其他 4xx（410 Gone 等）：資源用不了，但不是「回應格式壞掉」
    let (r, _) =
        one_status(ResponseTemplate::new(410).set_body_json(json!({"message": "Gone"}))).await;
    assert!(matches!(r, Err(ApiError::Forbidden { .. })), "{r:?}");

    // 伺服器錯誤算網路類（下一輪再試）
    let (r, _) = one_status(ResponseTemplate::new(502)).await;
    assert!(matches!(r, Err(ApiError::Network(_))), "{r:?}");

    // 200 但不是預期的 JSON
    let (r, _) = one_status(ResponseTemplate::new(200).set_body_string("not json")).await;
    assert!(matches!(r, Err(ApiError::Decode(_))), "{r:?}");
}

#[tokio::test]
async fn rate_limited_client_does_not_hit_network_until_reset() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "600"))
        .expect(1) // 第二次呼叫必須在用戶端就被擋下
        .mount(&server)
        .await;
    let c = client(&server);
    let first = c.get_json::<Item>("/x").await;
    let second = c.get_json::<Item>("/y").await;
    assert!(matches!(first, Err(ApiError::RateLimited { .. })));
    assert_eq!(first, second, "同一個 reset_at，且不再送請求");
}

#[tokio::test]
async fn error_body_is_never_returned_as_data() {
    // bash 版的教訓：403 的錯誤 JSON 不能被當成空清單或垃圾資料
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({"message": "Must have admin rights"})),
        )
        .mount(&server)
        .await;
    let r: Result<Vec<Item>, _> = client(&server)
        .get_paginated("/orgs/o/actions/runners", Some("runners"))
        .await;
    assert_eq!(
        r,
        Err(ApiError::Forbidden {
            message: "Must have admin rights".into()
        })
    );
}

#[tokio::test]
async fn set_token_changes_authorization_for_following_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"n": 1})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let c = client(&server);
    assert_eq!(c.get_json::<Item>("/x").await, Err(ApiError::Unauthorized));
    c.set_token("new".into());
    assert!(c.get_json::<Item>("/x").await.is_ok());
}

#[tokio::test]
async fn connection_failure_is_network_error() {
    let c = GitHubClient::new(Url::parse("http://127.0.0.1:1").unwrap(), "t".into());
    let r = c.get_json::<Item>("/x").await;
    assert!(matches!(r, Err(ApiError::Network(_))), "{r:?}");
    assert_eq!(c.rate().used_this_hour, 0, "沒有回應不計費");
}

#[tokio::test]
async fn halted_client_sends_no_requests_and_resumes_when_cleared() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"n": 1})))
        .mount(&server)
        .await;
    let halt = Arc::new(AtomicBool::new(false));
    let c =
        GitHubClient::new(Url::parse(&server.uri()).unwrap(), "tok".into()).with_halt(halt.clone());
    assert!(c.get_json::<Item>("/x").await.is_ok());

    halt.store(true, Ordering::SeqCst);
    let r = c.get_json::<Item>("/x").await;
    assert!(matches!(r, Err(ApiError::Network(_))), "{r:?}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "暫停後不再碰網路"
    );

    halt.store(false, Ordering::SeqCst);
    assert!(c.get_json::<Item>("/x").await.is_ok());
}

/// 實機上約每 8 輪就有 1 輪整輪失敗：重用的閒置連線已被對方關掉，請求一送出就斷
/// （`hyper::Error(IncompleteMessage)`）。這種失敗要換一條連線重送一次，不能讓整輪變成「離線」。
#[tokio::test]
async fn resends_once_when_the_connection_closes_before_any_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // 第一條連線：讀完請求就關掉，什麼都不回
        let (mut first, _) = listener.accept().await.unwrap();
        let _ = first.read(&mut [0u8; 4096]).await;
        drop(first);
        // 第二條連線：正常回應
        let (mut second, _) = listener.accept().await.unwrap();
        let _ = second.read(&mut [0u8; 4096]).await;
        let body = r#"{"n":7}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        second.write_all(resp.as_bytes()).await.unwrap();
    });
    let c = GitHubClient::new(Url::parse(&format!("http://{addr}")).unwrap(), "tok".into());
    let got: Item = c.get_json("/thing").await.expect("換一條連線重送後要成功");
    assert_eq!(got, Item { n: 7 });
}
