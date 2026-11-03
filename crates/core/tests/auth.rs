use pr_runners_core::auth::*;
use pr_runners_core::secrets::{MemoryStore, SecretStore};
use serde_json::json;
use url::Url;
use wiremock::matchers::{body_string_contains, header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NOW: i64 = 1_800_000_000;
fn now() -> i64 {
    NOW
}
fn flow(server: &MockServer) -> DeviceFlow {
    DeviceFlow::with_clock(Url::parse(&server.uri()).unwrap(), "Iv1.testclient", now)
}
fn code() -> DeviceCode {
    DeviceCode {
        device_code: "dev123".into(),
        user_code: "WDJB-MJHT".into(),
        verification_uri: "https://github.com/login/device".into(),
        interval: 5,
        expires_at: NOW + 900,
    }
}
/// 沒帶 `Accept: application/json` 時，GitHub 回 form-encoded；這個 mock 只在帶了的時候回 JSON，
/// 其他一律回 form-encoded（讓 JSON 解析失敗）。
async fn json_only(server: &MockServer, p: &str, matcher: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(p))
        .and(header("accept", "application/json"))
        .and(header_regex("user-agent", "^pr-runners/"))
        .and(body_string_contains(matcher))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
async fn form_encoded_fallback(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("error=bad&x=1", "application/x-www-form-urlencoded"),
        )
        .with_priority(10)
        .mount(server)
        .await;
}

#[tokio::test]
async fn start_returns_user_code() {
    let s = MockServer::start().await;
    json_only(&s, "/login/device/code", "client_id=Iv1.testclient",
        json!({"device_code": "dev123", "user_code": "WDJB-MJHT", "verification_uri": "https://github.com/login/device", "expires_in": 900, "interval": 5})).await;
    form_encoded_fallback(&s).await;
    assert_eq!(flow(&s).start().await.unwrap(), code());
}

#[tokio::test]
async fn start_reports_device_flow_disabled() {
    let s = MockServer::start().await;
    json_only(&s, "/login/device/code", "client_id", json!({"error": "device_flow_disabled", "error_description": "Device Flow must be explicitly enabled"})).await;
    let r = flow(&s).start().await;
    assert!(
        matches!(&r, Err(AuthError::Protocol(m)) if m.contains("device_flow_disabled")),
        "{r:?}"
    );
}

#[tokio::test]
async fn poll_pending_then_slowdown_then_done() {
    let s = MockServer::start().await;
    let grant = "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code";
    for (body, n) in [
        (json!({"error": "authorization_pending"}), 1),
        (json!({"error": "slow_down", "interval": 10}), 1),
    ] {
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(header("accept", "application/json"))
            .and(body_string_contains(grant))
            .and(body_string_contains("device_code=dev123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .up_to_n_times(n)
            .mount(&s)
            .await;
    }
    json_only(&s, "/login/oauth/access_token", grant, json!({
        "access_token": "ghu_x", "expires_in": 28_800, "refresh_token": "ghr_y", "refresh_token_expires_in": 15_811_200, "token_type": "bearer"})).await;

    let f = flow(&s);
    assert_eq!(f.poll(&code()).await.unwrap(), PollOutcome::Pending);
    assert_eq!(
        f.poll(&code()).await.unwrap(),
        PollOutcome::SlowDown { new_interval: 10 }
    );
    assert_eq!(
        f.poll(&code()).await.unwrap(),
        PollOutcome::Done(TokenSet {
            access_token: "ghu_x".into(),
            expires_at: Some(NOW + 28_800),
            refresh_token: Some("ghr_y".into()),
            refresh_expires_at: Some(NOW + 15_811_200),
        })
    );
}

#[tokio::test]
async fn slow_down_without_interval_adds_five_seconds() {
    let s = MockServer::start().await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "device_code",
        json!({"error": "slow_down"}),
    )
    .await;
    assert_eq!(
        flow(&s).poll(&code()).await.unwrap(),
        PollOutcome::SlowDown { new_interval: 10 }
    );
}

#[tokio::test]
async fn poll_access_denied() {
    let s = MockServer::start().await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "device_code",
        json!({"error": "access_denied"}),
    )
    .await;
    assert_eq!(flow(&s).poll(&code()).await.unwrap(), PollOutcome::Denied);
}

#[tokio::test]
async fn poll_expired_token() {
    let s = MockServer::start().await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "device_code",
        json!({"error": "expired_token"}),
    )
    .await;
    assert_eq!(flow(&s).poll(&code()).await.unwrap(), PollOutcome::Expired);
}

#[tokio::test]
async fn http_200_with_oauth_error_is_never_a_token() {
    let s = MockServer::start().await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "device_code",
        json!({"error": "incorrect_client_credentials", "error_description": "bad id"}),
    )
    .await;
    let r = flow(&s).poll(&code()).await;
    assert!(
        matches!(&r, Err(AuthError::Protocol(m)) if m.contains("incorrect_client_credentials")),
        "{r:?}"
    );
}

#[tokio::test]
async fn refresh_sends_no_client_secret() {
    let s = MockServer::start().await;
    json_only(&s, "/login/oauth/access_token", "grant_type=refresh_token",
        json!({"access_token": "ghu_new", "expires_in": 28_800, "refresh_token": "ghr_new", "refresh_token_expires_in": 15_811_200})).await;
    let t = flow(&s).refresh("ghr_old").await.unwrap();
    assert_eq!(t.access_token, "ghu_new");
    assert_eq!(t.refresh_token.as_deref(), Some("ghr_new"));
    assert_eq!(t.expires_at, Some(NOW + 28_800));
    let reqs = s.received_requests().await.unwrap();
    let body = String::from_utf8(reqs[0].body.clone()).unwrap();
    assert!(!body.contains("client_secret"), "{body}");
    assert!(
        body.contains("grant_type=refresh_token")
            && body.contains("refresh_token=ghr_old")
            && body.contains("client_id=Iv1.testclient"),
        "{body}"
    );
}

#[tokio::test]
async fn requests_ask_for_json() {
    // 沒帶 Accept: application/json 就會拿到 form-encoded → 解析失敗。三個呼叫都要成功。
    let s = MockServer::start().await;
    json_only(&s, "/login/device/code", "client_id", json!({"device_code": "d", "user_code": "u", "verification_uri": "v", "expires_in": 900, "interval": 5})).await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "device_code",
        json!({"access_token": "t"}),
    )
    .await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "refresh_token",
        json!({"access_token": "t2"}),
    )
    .await;
    form_encoded_fallback(&s).await;
    let f = flow(&s);
    f.start().await.unwrap();
    assert!(matches!(
        f.poll(&code()).await.unwrap(),
        PollOutcome::Done(_)
    ));
    assert_eq!(f.refresh("r").await.unwrap().access_token, "t2");
}

#[tokio::test]
async fn refresh_error_classification() {
    let s = MockServer::start().await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "refresh_token=dead",
        json!({"error": "bad_refresh_token"}),
    )
    .await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "refresh_token=odd",
        json!({"error": "unauthorized_client"}),
    )
    .await;
    let f = flow(&s);
    assert_eq!(f.refresh("dead").await, Err(AuthError::InvalidGrant));
    assert!(
        matches!(f.refresh("odd").await, Err(AuthError::Protocol(_))),
        "只有 bad_refresh_token 才是 InvalidGrant"
    );

    // 連線失敗 → Network（帳號暫時離線，不要求重新登入）
    let down = DeviceFlow::with_clock(Url::parse("http://127.0.0.1:1").unwrap(), "id", now);
    assert!(matches!(
        down.refresh("x").await,
        Err(AuthError::Network(_))
    ));

    // 伺服器 5xx 也是 Network
    let s5 = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&s5)
        .await;
    assert!(matches!(
        flow(&s5).refresh("x").await,
        Err(AuthError::Network(_))
    ));
}

fn stored(expires_at: Option<i64>, refresh: Option<&str>) -> TokenSet {
    TokenSet {
        access_token: "old".into(),
        expires_at,
        refresh_token: refresh.map(str::to_string),
        refresh_expires_at: None,
    }
}

#[tokio::test]
async fn refresh_if_needed_replaces_the_stored_pair() {
    let s = MockServer::start().await;
    json_only(&s, "/login/oauth/access_token", "refresh_token=ghr_old",
        json!({"access_token": "ghu_new", "expires_in": 28_800, "refresh_token": "ghr_new", "refresh_token_expires_in": 15_811_200})).await;
    let store = MemoryStore::new();
    store
        .put(1, &stored(Some(NOW + 100), Some("ghr_old")))
        .unwrap();
    let out = refresh_if_needed(&flow(&s), &store, 1, NOW).await;
    assert_eq!(
        out,
        RefreshOutcome::Refreshed {
            access_token: "ghu_new".into()
        }
    );
    let now_stored = store.get(1).unwrap().unwrap();
    assert_eq!(now_stored.access_token, "ghu_new");
    assert_eq!(
        now_stored.refresh_token.as_deref(),
        Some("ghr_new"),
        "舊的 refresh token 已失效，必須被取代"
    );
}

#[tokio::test]
async fn refresh_if_needed_skips_when_not_due_or_pat() {
    let s = MockServer::start().await; // 沒有任何 mock：被呼叫就會 404 → 測試會抓到
    let store = MemoryStore::new();
    store
        .put(1, &stored(Some(NOW + 10_000), Some("r")))
        .unwrap();
    store.put(2, &stored(None, None)).unwrap(); // PAT
    assert_eq!(
        refresh_if_needed(&flow(&s), &store, 1, NOW).await,
        RefreshOutcome::NotNeeded
    );
    assert_eq!(
        refresh_if_needed(&flow(&s), &store, 2, NOW).await,
        RefreshOutcome::NotNeeded
    );
    assert!(s.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn refresh_if_needed_classifies_failures() {
    let s = MockServer::start().await;
    json_only(
        &s,
        "/login/oauth/access_token",
        "refresh_token=dead",
        json!({"error": "bad_refresh_token"}),
    )
    .await;
    let store = MemoryStore::new();
    store.put(1, &stored(Some(NOW), Some("dead"))).unwrap();
    assert_eq!(
        refresh_if_needed(&flow(&s), &store, 1, NOW).await,
        RefreshOutcome::NeedsReauth
    );
    // 金鑰庫裡根本沒有 token
    assert_eq!(
        refresh_if_needed(&flow(&s), &store, 99, NOW).await,
        RefreshOutcome::NeedsReauth
    );
    // 斷線：不要求重新登入，舊 token 原樣保留
    let down = DeviceFlow::with_clock(Url::parse("http://127.0.0.1:1").unwrap(), "id", now);
    store.put(2, &stored(Some(NOW), Some("r"))).unwrap();
    assert_eq!(
        refresh_if_needed(&down, &store, 2, NOW).await,
        RefreshOutcome::Offline
    );
    assert_eq!(store.get(2).unwrap().unwrap().access_token, "old");
}
