//! GitHub REST 用戶端：ETag 快取、分頁、配額記錄、錯誤分類。
//!
//! 只做 GET。回應若不是成功狀態一律回 `Err`——錯誤內容（例如 403 的
//! `{"message":"Must have admin rights"}`）絕不會被當成資料交出去，
//! bash 版就是因此把忙碌 runner 顯示成閒置。

use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

use crate::transport::{self, Transport};

/// 一小時的計費請求窗。
const WINDOW_SECS: i64 = 3_600;
/// secondary 限流沒給等待標頭時，至少等這麼久。
const SECONDARY_MIN_WAIT_SECS: i64 = 60;
/// secondary 連續失敗的退避上限。
const SECONDARY_MAX_WAIT_SECS: i64 = 3_600;
/// ETag 快取上限：超過就淘汰最舊的（長時間執行的選單列 App 不能無限長大）。
const MAX_CACHE_ENTRIES: usize = 2_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateState {
    /// `X-RateLimit-Limit`（還沒收過任何回應時為 0＝未知）
    pub limit: u32,
    /// `X-RateLimit-Remaining`
    pub remaining: u32,
    /// `X-RateLimit-Reset`（Unix 秒）
    pub reset_at: i64,
    /// 這台裝置最近一小時實際送出且會被計費的請求數（本機估算，不持久化）。
    pub used_this_hour: u32,
}

impl RateState {
    /// 剩餘量低於上限的 20%：要拉長輪詢間隔。還沒收過任何回應（limit＝0）不算。
    pub fn is_low(&self) -> bool {
        self.limit > 0 && self.remaining < self.limit / 5
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error("token 無效或已撤銷（401）")]
    Unauthorized,
    #[error("沒有權限（403）：{message}")]
    Forbidden { message: String },
    #[error("找不到（404）")]
    NotFound,
    /// 被限流（primary 或 secondary）。`reset_at` 是最早可重試的時間，
    /// 在那之前用戶端不會再送請求。
    #[error("被限流，最早 {reset_at}（Unix 秒）可重試")]
    RateLimited { reset_at: i64 },
    #[error("網路錯誤：{0}")]
    Network(String),
    #[error("回應無法解讀：{0}")]
    Decode(String),
}

impl ApiError {
    /// 這些錯誤代表整個帳號有事（token 沒了、被限流、斷線、回應格式不對），
    /// 不能當成「這個 repo／group 資料不明」吞掉而讓畫面靜默停在舊資料。
    pub fn is_account_wide(&self) -> bool {
        matches!(
            self,
            ApiError::Unauthorized
                | ApiError::RateLimited { .. }
                | ApiError::Network(_)
                | ApiError::Decode(_)
        )
    }
}

struct CacheEntry {
    etag: String,
    body: Bytes,
    next: Option<Url>,
}

#[derive(Default)]
struct Meter {
    rate: RateState,
    /// 最近一小時內計費請求的時間（Unix 秒）
    billed: VecDeque<i64>,
    /// 限流期間：這個時間之前不送任何請求
    blocked_until: i64,
    /// 連續 secondary 限流次數，用來指數退避
    secondary_streak: u32,
    /// 啟動以來計費請求的累計數（收集器用它算每一輪的成本）
    billed_total: u64,
}

#[derive(Default)]
struct EtagCache {
    entries: HashMap<Url, CacheEntry>,
    /// 插入順序，滿了先淘汰最舊的（整個清空會讓下一輪全部變成計費的 200）
    order: VecDeque<Url>,
}

impl EtagCache {
    fn insert(&mut self, url: Url, entry: CacheEntry) {
        if self.entries.insert(url.clone(), entry).is_none() {
            self.order.push_back(url);
        }
        while self.entries.len() > MAX_CACHE_ENTRIES {
            match self.order.pop_front() {
                Some(old) => {
                    self.entries.remove(&old);
                }
                None => break,
            }
        }
    }
}

pub struct GitHubClient {
    transport: Transport,
    /// 設了就不再發任何請求（Android 切到背景）：進行中的掃描會在下一個請求處中止。
    halt: Option<Arc<AtomicBool>>,
    base: Url,
    token: RwLock<String>,
    cache: Mutex<EtagCache>,
    meter: Mutex<Meter>,
}

/// 現在的 Unix 秒（UTC）。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 連續第 `streak` 次 secondary 限流（從 1 起算）要等多久：60、120、240…，上限一小時。
pub fn secondary_wait_secs(streak: u32) -> i64 {
    let shift = streak.saturating_sub(1).min(16);
    (SECONDARY_MIN_WAIT_SECS << shift).min(SECONDARY_MAX_WAIT_SECS)
}

impl GitHubClient {
    /// `api_base` 可指向 wiremock；結尾沒有 `/` 會補上。
    pub fn new(mut api_base: Url, token: String) -> Self {
        if !api_base.path().ends_with('/') {
            let p = format!("{}/", api_base.path());
            api_base.set_path(&p);
        }
        Self {
            transport: Transport::new(),
            halt: None,
            base: api_base,
            token: RwLock::new(token),
            cache: Mutex::new(EtagCache::default()),
            meter: Mutex::new(Meter::default()),
        }
    }

    /// 共用的暫停旗標：設為 true 後，這個 client 的後續請求會直接失敗（不碰網路）。
    pub fn with_halt(mut self, halt: Arc<AtomicBool>) -> Self {
        self.halt = Some(halt);
        self
    }

    pub fn set_token(&self, token: String) {
        *self.token.write().expect("token lock") = token;
    }

    pub fn rate(&self) -> RateState {
        let mut m = self.meter.lock().expect("meter lock");
        prune(&mut m.billed, now_secs());
        RateState {
            used_this_hour: m.billed.len() as u32,
            ..m.rate
        }
    }

    /// 被限流時，最早可以再送請求的時間（Unix 秒）；沒被限流回傳 None。
    pub fn blocked_until(&self) -> Option<i64> {
        let m = self.meter.lock().expect("meter lock");
        (m.blocked_until > now_secs()).then_some(m.blocked_until)
    }

    /// 啟動以來送出且計費的請求累計數（不會因一小時窗滑動而減少）。
    pub fn billed_total(&self) -> u64 {
        self.meter.lock().expect("meter lock").billed_total
    }

    /// 取一個 JSON 物件（或陣列）。
    pub async fn get_json<T: DeserializeOwned>(&self, path_and_query: &str) -> Result<T, ApiError> {
        let url = self.url(path_and_query)?;
        let page = self.fetch(&url).await?;
        serde_json::from_slice(&page.body).map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// 跟著 `Link: rel="next"` 走到底。`items_key` 用於 `{"runners":[...]}` 這類包裝。
    pub async fn get_paginated<T: DeserializeOwned>(
        &self,
        path_and_query: &str,
        items_key: Option<&str>,
    ) -> Result<Vec<T>, ApiError> {
        let mut url = self.url(path_and_query)?;
        let mut all = Vec::new();
        loop {
            let page = self.fetch(&url).await?;
            let value: serde_json::Value =
                serde_json::from_slice(&page.body).map_err(|e| ApiError::Decode(e.to_string()))?;
            let items = match items_key {
                Some(key) => value
                    .get(key)
                    .cloned()
                    .ok_or_else(|| ApiError::Decode(format!("回應裡沒有 `{key}`")))?,
                None => value,
            };
            let mut batch: Vec<T> =
                serde_json::from_value(items).map_err(|e| ApiError::Decode(e.to_string()))?;
            all.append(&mut batch);
            match page.next {
                Some(next) => url = next,
                None => return Ok(all),
            }
        }
    }

    fn url(&self, path_and_query: &str) -> Result<Url, ApiError> {
        self.base
            .join(path_and_query.trim_start_matches('/'))
            .map_err(|e| ApiError::Decode(format!("網址不合法：{e}")))
    }

    fn request(&self, http: &reqwest::Client, url: &Url) -> reqwest::RequestBuilder {
        let token = self.token.read().expect("token lock").clone();
        let req = http
            .get(url.clone())
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        let cached_etag = self
            .cache
            .lock()
            .expect("cache lock")
            .entries
            .get(url)
            .map(|e| e.etag.clone());
        match cached_etag {
            Some(etag) => req.header("If-None-Match", etag),
            None => req,
        }
    }

    /// 每一次送出前都要成立的條件：沒有被暫停（App 在背景）、沒有被限流。
    fn may_send(&self) -> Result<(), ApiError> {
        if self.halt.as_ref().is_some_and(|h| h.load(Ordering::SeqCst)) {
            return Err(ApiError::Network("已暫停（App 在背景）".into()));
        }
        let m = self.meter.lock().expect("meter lock");
        if m.blocked_until > now_secs() {
            return Err(ApiError::RateLimited {
                reset_at: m.blocked_until,
            });
        }
        Ok(())
    }

    /// 丟掉所有閒置連線（App 回到前景時呼叫：裝置睡眠期間閒置時間不累計，過期的連線看起來還很新）。
    pub fn reset_connections(&self) {
        self.transport.reset();
    }

    async fn fetch(&self, url: &Url) -> Result<Page, ApiError> {
        // 先拿到在途名額再檢查：排隊期間 App 可能已切到背景、或別的請求剛被限流
        let _slot = self.transport.slot().await;
        self.may_send()?;
        let resp = match self.transport.send(|http| self.request(http, url)).await {
            // 重用的連線其實已經斷了：同一池的其他閒置連線多半也一樣，整池換新後重送一次
            Err(e) if transport::reused_connection_closed(&e) => {
                self.transport.reset();
                self.may_send()?;
                self.transport.send(|http| self.request(http, url)).await
            }
            other => other,
        }
        .map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let now = now_secs();

        // 經授權的 304 不計入 primary 計費；其餘有回應的請求都算一次。
        self.record(&headers, now, status.as_u16() != 304);

        if status.as_u16() == 304 {
            let cache = self.cache.lock().expect("cache lock");
            return match cache.entries.get(url) {
                Some(e) => Ok(Page {
                    body: e.body.clone(),
                    next: e.next.clone(),
                }),
                None => Err(ApiError::Decode("收到 304 但沒有快取".into())),
            };
        }
        if status.is_success() {
            self.meter.lock().expect("meter lock").secondary_streak = 0;
            let next = self.next_link(&headers)?;
            if let Some(etag) = header_str(&headers, "etag") {
                let mut cache = self.cache.lock().expect("cache lock");
                cache.insert(
                    url.clone(),
                    CacheEntry {
                        etag: etag.to_string(),
                        body: body.clone(),
                        next: next.clone(),
                    },
                );
            }
            return Ok(Page { body, next });
        }
        Err(self.classify(status.as_u16(), &headers, &body, now))
    }

    fn record(&self, headers: &reqwest::header::HeaderMap, now: i64, billed: bool) {
        let mut m = self.meter.lock().expect("meter lock");
        if let Some(v) = header_num(headers, "x-ratelimit-limit") {
            m.rate.limit = v as u32;
        }
        if let Some(v) = header_num(headers, "x-ratelimit-remaining") {
            m.rate.remaining = v as u32;
        }
        if let Some(v) = header_num(headers, "x-ratelimit-reset") {
            m.rate.reset_at = v;
        }
        prune(&mut m.billed, now);
        if billed {
            m.billed.push_back(now);
            m.billed_total += 1;
        }
    }

    fn classify(
        &self,
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: &[u8],
        now: i64,
    ) -> ApiError {
        let message = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str().map(str::to_string))
            })
            .unwrap_or_default();
        match status {
            401 => ApiError::Unauthorized,
            404 => ApiError::NotFound,
            403 | 429 => {
                let remaining_zero = header_num(headers, "x-ratelimit-remaining") == Some(0);
                let retry_after = header_num(headers, "retry-after");
                let says_limit = message.to_ascii_lowercase().contains("rate limit");
                if status == 403 && !remaining_zero && retry_after.is_none() && !says_limit {
                    return ApiError::Forbidden { message };
                }
                // 兩種等待並存時取較晚者
                let mut until = 0;
                if remaining_zero && let Some(reset) = header_num(headers, "x-ratelimit-reset") {
                    until = until.max(reset);
                }
                if let Some(secs) = retry_after {
                    until = until.max(now + secs);
                }
                let mut m = self.meter.lock().expect("meter lock");
                if retry_after.is_none() && !remaining_zero {
                    // secondary 限流沒給等待標頭：至少 60 秒，連續失敗指數退避
                    m.secondary_streak += 1;
                    until = until.max(now + secondary_wait_secs(m.secondary_streak));
                }
                let until = until.max(now + 1);
                m.blocked_until = m.blocked_until.max(until);
                ApiError::RateLimited { reset_at: until }
            }
            500..=599 => ApiError::Network(format!("GitHub 回 HTTP {status}")),
            // 其他 4xx（410 Gone、422、451…）：這個資源用不了，但不是回應格式壞掉
            _ => ApiError::Forbidden {
                message: format!("HTTP {status}：{message}"),
            },
        }
    }

    /// 只跟著同一個來源的 next 連結，免得 token 被帶去別的主機。
    fn next_link(&self, headers: &reqwest::header::HeaderMap) -> Result<Option<Url>, ApiError> {
        let Some(link) = header_str(headers, "link") else {
            return Ok(None);
        };
        let Some(next) = parse_next_link(link) else {
            return Ok(None);
        };
        let url = Url::parse(&next).map_err(|e| ApiError::Decode(format!("Link 不合法：{e}")))?;
        if url.origin() != self.base.origin() {
            return Err(ApiError::Decode(format!("Link 指向別的來源：{url}")));
        }
        Ok(Some(url))
    }
}

struct Page {
    body: Bytes,
    next: Option<Url>,
}

fn prune(billed: &mut VecDeque<i64>, now: i64) {
    while billed.front().is_some_and(|t| now - *t >= WINDOW_SECS) {
        billed.pop_front();
    }
}

fn header_str<'a>(h: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name)?.to_str().ok()
}

fn header_num(h: &reqwest::header::HeaderMap, name: &str) -> Option<i64> {
    header_str(h, name)?.trim().parse().ok()
}

/// `<https://…?page=2>; rel="next", <…>; rel="last"` → 第一個 rel="next" 的網址。
fn parse_next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| p.trim().replace(' ', "") == "rel=\"next\"");
        let target = target.trim();
        (is_next && target.starts_with('<') && target.ends_with('>'))
            .then(|| target[1..target.len() - 1].to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_next_among_other_rels() {
        let l = "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=9>; rel=\"last\"";
        assert_eq!(
            parse_next_link(l).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
        assert_eq!(parse_next_link("<https://a/b>; rel=\"last\""), None);
        assert_eq!(parse_next_link("garbage"), None);
    }

    #[test]
    fn etag_cache_evicts_oldest_instead_of_clearing_everything() {
        let entry = || CacheEntry {
            etag: "e".into(),
            body: Bytes::new(),
            next: None,
        };
        let url = |i: usize| Url::parse(&format!("https://api.github.com/x/{i}")).unwrap();
        let mut c = EtagCache::default();
        for i in 0..MAX_CACHE_ENTRIES + 10 {
            c.insert(url(i), entry());
        }
        assert_eq!(c.entries.len(), MAX_CACHE_ENTRIES);
        assert!(!c.entries.contains_key(&url(0)), "最舊的被淘汰");
        assert!(c.entries.contains_key(&url(10)), "第 11 筆起仍在");
        assert!(
            c.entries.contains_key(&url(MAX_CACHE_ENTRIES + 9)),
            "最新的在"
        );
        // 重複寫入同一個網址不重複排隊
        c.insert(url(MAX_CACHE_ENTRIES + 9), entry());
        assert_eq!(c.order.len(), MAX_CACHE_ENTRIES);
    }

    #[test]
    fn secondary_backoff_doubles_and_caps() {
        assert_eq!(secondary_wait_secs(1), 60);
        assert_eq!(secondary_wait_secs(2), 120);
        assert_eq!(secondary_wait_secs(3), 240);
        assert_eq!(secondary_wait_secs(6), 1_920);
        assert_eq!(secondary_wait_secs(7), 3_600);
        assert_eq!(secondary_wait_secs(500), 3_600);
    }
}
