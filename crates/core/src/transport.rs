//! 連到 GitHub 的連線層：逾時、閒置連線怎麼處理、同時在途的上限、整池換新。
//! `GitHubClient` 管 REST 語意（ETag、分頁、配額、錯誤分類、何時重送），連線怎麼建立與重用都在這裡。

use std::sync::RwLock;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

/// 同一個 token 同時在途的請求上限。GitHub 的 secondary 限流看的是同時請求數，
/// 所以上限放在所有請求都會經過的這一層，呼叫端可以放心把獨立的請求並行送出。
const MAX_IN_FLIGHT: usize = 8;
/// 閒置超過這麼久的連線不再重用。要比最短的輪詢間隔短（`collector` 有編譯期檢查）：
/// 閒置跨過一輪的連線常已被對方或中間的網路設備關掉（直接斷 → 送出就失敗；默默丟掉 → 要等到逾時）。
pub(crate) const IDLE_CONNECTION_SECS: u64 = 20;

/// 所有連到 GitHub 的 HTTP client 都從這裡建（API 請求與 OAuth 裝置登入／續期共用同一組設定）。
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        // GitHub 拒收沒有 User-Agent 的請求，reqwest 預設不帶
        .user_agent(concat!("pr-runners/", env!("CARGO_PKG_VERSION")))
        // 沒有逾時的話，一條卡住的連線會讓整輪掃描（與持有收集器或帳號生命週期鎖的人）永遠卡住
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(IDLE_CONNECTION_SECS))
        .build()
        .expect("reqwest client")
}

pub(crate) struct Transport {
    /// 目前的連線池。整池換新時換掉這一份；已經在送的請求拿的是舊的那份，照樣送完。
    pool: RwLock<reqwest::Client>,
    in_flight: Semaphore,
}

impl Transport {
    pub(crate) fn new() -> Self {
        Self {
            pool: RwLock::new(http_client()),
            in_flight: Semaphore::new(MAX_IN_FLIGHT),
        }
    }

    /// 等到一個在途名額；送出請求到讀完回應都要持有它。
    pub(crate) async fn slot(&self) -> SemaphorePermit<'_> {
        self.in_flight
            .acquire()
            .await
            .expect("semaphore is never closed")
    }

    /// 用目前的連線池組出請求並送出。
    pub(crate) async fn send(
        &self,
        build: impl FnOnce(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> reqwest::Result<reqwest::Response> {
        let pool = self.pool.read().expect("pool lock").clone();
        build(&pool).send().await
    }

    /// 丟掉所有閒置連線，之後的請求一律開新連線。用在「重用的連線其實已經斷了」（同一池裡的其他閒置連線
    /// 多半也一樣，例如剛換網路）與「App 剛回到前景」（裝置睡眠期間閒置時間不會累計，過期的連線看起來還很新）。
    pub(crate) fn reset(&self) {
        *self.pool.write().expect("pool lock") = http_client();
    }
}

/// 請求在送出階段就失敗、而且不是連不上也不是逾時：幾乎都是重用了一條已被對方（或中間的網路設備）
/// 關掉的連線。這種失敗換新連線重送一次就好；只做 GET，重送沒有副作用。
pub(crate) fn reused_connection_closed(e: &reqwest::Error) -> bool {
    e.is_request() && !e.is_connect() && !e.is_timeout()
}
