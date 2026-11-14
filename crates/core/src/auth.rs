//! GitHub Device Flow 登入與 token 續期。
//!
//! - 兩個 POST（`/login/device/code`、`/login/oauth/access_token`）一律帶 `Accept: application/json`
//!   （不帶的話 GitHub 回 form-encoded，JSON 解析會失敗）與 `User-Agent`。
//! - HTTP 200 仍可能含 OAuth `error`，先依錯誤分類，不當成成功的 `TokenSet`。
//! - refresh 不送 client_secret（Device Flow 的 App 只內嵌 client ID）。
//! - 只有 `bad_refresh_token` 才是 `InvalidGrant`（要重新登入）；斷線是 `Network`（下一輪再試）。

use serde::{Deserialize, Serialize};
use std::fmt;
use url::Url;

/// 建置時從環境變數 `PR_RUNNERS_GITHUB_CLIENT_ID` 讀；沒設就是 None（UI 顯示「此版本未設定 GitHub App」）。
pub const CLIENT_ID: Option<&str> = option_env!("PR_RUNNERS_GITHUB_CLIENT_ID");

/// 離到期還剩不到這麼久就先續期。
const REFRESH_MARGIN_SECS: i64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// refresh token 無效或已過期（`bad_refresh_token`）→ 帳號標 NeedsReauth
    #[error("refresh token 無效或已過期")]
    InvalidGrant,
    /// 連線失敗或 GitHub 伺服器錯誤 → 帳號暫時離線，下一輪再試
    #[error("網路錯誤：{0}")]
    Network(String),
    /// 其他 OAuth 錯誤或無法解讀的回應
    #[error("登入流程錯誤：{0}")]
    Protocol(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// 輪詢間隔（秒）
    pub interval: u64,
    /// 代碼到期時間（Unix 秒）
    pub expires_at: i64,
}

/// 存進金鑰庫的就是它的 JSON。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub expires_at: Option<i64>,
    pub refresh_token: Option<String>,
    pub refresh_expires_at: Option<i64>,
}

impl fmt::Debug for TokenSet {
    /// 不把 token 印進 log。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Pending,
    /// 之後改用這個間隔輪詢
    SlowDown {
        new_interval: u64,
    },
    Done(TokenSet),
    Denied,
    Expired,
}

/// 有 refresh token 且有 `expires_at` 才判斷；PAT 不走 refresh。
pub fn needs_refresh(t: &TokenSet, now: i64) -> bool {
    t.refresh_token.is_some()
        && t.expires_at
            .is_some_and(|exp| exp - now < REFRESH_MARGIN_SECS)
}

pub struct DeviceFlow {
    /// 預設 `https://github.com`
    pub web_base: Url,
    pub client_id: String,
    http: reqwest::Client,
    now: fn() -> i64,
}

#[derive(Deserialize)]
struct OAuthBody {
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
    // device code
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    expires_in: Option<i64>,
    // token
    access_token: Option<String>,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<i64>,
}

impl DeviceFlow {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self::with_base(
            Url::parse("https://github.com").expect("static url"),
            client_id,
        )
    }

    pub fn with_base(web_base: Url, client_id: impl Into<String>) -> Self {
        Self::with_clock(web_base, client_id, crate::client::now_secs)
    }

    pub fn with_clock(web_base: Url, client_id: impl Into<String>, now: fn() -> i64) -> Self {
        // 與 API 請求同一組連線設定：沒有逾時的話，一次卡住的續期會讓輪詢與移除帳號（同一把生命週期鎖）永遠等下去
        let http = crate::transport::http_client();
        Self {
            web_base,
            client_id: client_id.into(),
            http,
            now,
        }
    }

    async fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<OAuthBody, AuthError> {
        let url = self
            .web_base
            .join(path)
            .map_err(|e| AuthError::Protocol(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            .header("Accept", "application/json")
            .form(form)
            .send()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        let status = resp.status();
        if status.is_server_error() {
            return Err(AuthError::Network(format!("GitHub 回 HTTP {status}")));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| AuthError::Protocol(format!("HTTP {status}，回應不是 JSON：{e}")))
    }

    fn protocol_error(b: &OAuthBody) -> AuthError {
        AuthError::Protocol(format!(
            "{}{}",
            b.error.as_deref().unwrap_or("unknown_error"),
            b.error_description
                .as_deref()
                .map(|d| format!("：{d}"))
                .unwrap_or_default()
        ))
    }

    pub async fn start(&self) -> Result<DeviceCode, AuthError> {
        let b = self
            .post("/login/device/code", &[("client_id", &self.client_id)])
            .await?;
        if b.error.is_some() {
            return Err(Self::protocol_error(&b));
        }
        let missing = || AuthError::Protocol("device code 回應缺欄位".into());
        Ok(DeviceCode {
            device_code: b.device_code.ok_or_else(missing)?,
            user_code: b.user_code.ok_or_else(missing)?,
            verification_uri: b.verification_uri.ok_or_else(missing)?,
            interval: b.interval.unwrap_or(5),
            expires_at: (self.now)() + b.expires_in.ok_or_else(missing)?,
        })
    }

    pub async fn poll(&self, code: &DeviceCode) -> Result<PollOutcome, AuthError> {
        let b = self
            .post(
                "/login/oauth/access_token",
                &[
                    ("client_id", &self.client_id),
                    ("device_code", &code.device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ],
            )
            .await?;
        match b.error.as_deref() {
            None => Ok(PollOutcome::Done(self.token_set(b)?)),
            Some("authorization_pending") => Ok(PollOutcome::Pending),
            // slow_down 之後的間隔是增加後的值（GitHub 的規則是原間隔 +5 秒）
            Some("slow_down") => Ok(PollOutcome::SlowDown {
                new_interval: b.interval.unwrap_or(code.interval + 5),
            }),
            Some("access_denied") => Ok(PollOutcome::Denied),
            Some("expired_token") => Ok(PollOutcome::Expired),
            Some(_) => Err(Self::protocol_error(&b)),
        }
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, AuthError> {
        // 不送 client_secret
        let b = self
            .post(
                "/login/oauth/access_token",
                &[
                    ("client_id", &self.client_id),
                    ("refresh_token", refresh_token),
                    ("grant_type", "refresh_token"),
                ],
            )
            .await?;
        match b.error.as_deref() {
            None => self.token_set(b),
            Some("bad_refresh_token") => Err(AuthError::InvalidGrant),
            Some(_) => Err(Self::protocol_error(&b)),
        }
    }

    /// GitHub 回的是相對秒數；這裡以收到回應的時間換成 Unix 秒。
    fn token_set(&self, b: OAuthBody) -> Result<TokenSet, AuthError> {
        let now = (self.now)();
        Ok(TokenSet {
            access_token: b
                .access_token
                .ok_or_else(|| AuthError::Protocol("回應沒有 access_token".into()))?,
            expires_at: b.expires_in.map(|s| now + s),
            refresh_token: b.refresh_token,
            refresh_expires_at: b.refresh_token_expires_in.map(|s| now + s),
        })
    }
}

/// 啟動、以及每一輪掃描之前，檢查一個帳號的 token 要不要續期。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// 不用續期（PAT，或離到期還久）
    NotNeeded,
    /// 續期成功，`access_token` 要換進 HTTP client。新的一組 token 已交給 `SecretStore::put`；
    /// 寫入失敗時由 `RetryingStore` 留在記憶體並在之後重試（舊的 refresh token 已經失效）。
    Refreshed { access_token: String },
    /// refresh token 無效／過期，或金鑰庫裡根本沒有 token：要重新登入
    NeedsReauth,
    /// 斷線或暫時性錯誤：帳號暫時離線，下一輪再試，不要求重新登入
    Offline,
}

/// 順序固定：refresh → 寫入金鑰庫 → 呼叫端換 HTTP client 的 token。
/// 續期成功會讓舊 access token 與舊 refresh token同時失效，所以新的一組必須取代所有舊的。
pub async fn refresh_if_needed(
    flow: &DeviceFlow,
    store: &dyn crate::secrets::SecretStore,
    account_id: u64,
    now: i64,
) -> RefreshOutcome {
    let token = match store.get(account_id) {
        Ok(Some(t)) => t,
        Ok(None) => return RefreshOutcome::NeedsReauth,
        Err(_) => return RefreshOutcome::Offline,
    };
    if !needs_refresh(&token, now) {
        return RefreshOutcome::NotNeeded;
    }
    let Some(refresh_token) = token.refresh_token.as_deref() else {
        return RefreshOutcome::NotNeeded;
    };
    match flow.refresh(refresh_token).await {
        Ok(new) => {
            // 寫入失敗也不能丟掉這組新 token：由 RetryingStore 保留並重試
            let _ = store.put(account_id, &new);
            RefreshOutcome::Refreshed {
                access_token: new.access_token,
            }
        }
        Err(AuthError::InvalidGrant) => RefreshOutcome::NeedsReauth,
        Err(AuthError::Network(_) | AuthError::Protocol(_)) => RefreshOutcome::Offline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(expires_at: Option<i64>, refresh: bool) -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            expires_at,
            refresh_token: refresh.then(|| "r".into()),
            refresh_expires_at: None,
        }
    }

    #[test]
    fn needs_refresh_five_minutes_before_expiry() {
        let now = 10_000;
        assert!(
            !needs_refresh(&t(Some(now + 300), true), now),
            "剛好 300 秒不算"
        );
        assert!(needs_refresh(&t(Some(now + 299), true), now));
        assert!(needs_refresh(&t(Some(now - 5), true), now), "已過期");
        assert!(!needs_refresh(&t(Some(now + 3_600), true), now));
        assert!(
            !needs_refresh(&t(Some(now + 1), false), now),
            "PAT／沒有 refresh token 不走 refresh"
        );
        assert!(!needs_refresh(&t(None, true), now), "沒有到期時間不判斷");
    }

    #[test]
    fn token_debug_never_prints_secrets() {
        let s = format!(
            "{:?}",
            TokenSet {
                access_token: "ghu_SECRET".into(),
                refresh_token: Some("ghr_SECRET".into()),
                ..t(None, false)
            }
        );
        assert!(!s.contains("SECRET"), "{s}");
    }
}
