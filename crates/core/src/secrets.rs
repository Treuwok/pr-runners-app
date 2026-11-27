//! token 只放系統金鑰庫（service `"pr-runners"`，user `"github:<client_id>:<user_id>"`），不寫進明文檔案。
//! 升級前的舊格式 `"github:<user_id>"` 讀得到（見 [`KeyringStore`]）。
//!
//! 金鑰庫用 keyring 4.2 官方建議的 `keyring-core` ＋平台 store：必須先建立平台 store 並
//! `keyring_core::set_default_store`，才能建立 `Entry`。不混用 keyring 4.2 預設的 `v1` Entry
//! （它自己設定桌面 store，但不支援 Android）。Mac 用 [`init_native_store`]；Android 的 store
//! 在 Tauri 提供 application context 之後建立（計畫 Task 11）。

use crate::auth::TokenSet;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("金鑰庫錯誤：{0}")]
    Store(String),
    #[error("金鑰庫裡的內容無法解讀：{0}")]
    Corrupt(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, account_id: u64) -> Result<Option<TokenSet>, SecretError>;
    fn put(&self, account_id: u64, t: &TokenSet) -> Result<(), SecretError>;
    fn delete(&self, account_id: u64) -> Result<(), SecretError>;
}

const SERVICE: &str = "pr-runners";

/// 舊格式的 key（沒有 client ID）。升級前存的 token 都在這裡。
fn legacy_user(account_id: u64) -> String {
    format!("github:{account_id}")
}

/// GitHub 會拒絕用別的 client ID 續期：refresh token 綁定發它的那個 GitHub App。
/// dev 版（`pr-runners-dev`）與正式版若共用同一個 key，後登入的一邊會蓋掉另一邊的 token，
/// 被蓋掉的那邊就再也續不了期。所以 key 帶上 client ID，各存各的。
fn user(client_id: Option<&str>, account_id: u64) -> String {
    match client_id {
        Some(c) => format!("github:{c}:{account_id}"),
        None => legacy_user(account_id),
    }
}

/// 系統金鑰庫。使用前必須已經設定 default store。
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore {
    /// 建置時的 GitHub App client ID；沒有（只用 PAT 的建置）就沿用舊格式的 key
    client_id: Option<&'static str>,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self {
            client_id: crate::auth::CLIENT_ID,
        }
    }

    #[cfg(test)]
    fn with_client_id(client_id: Option<&'static str>) -> Self {
        Self { client_id }
    }

    fn entry_for(user: &str) -> Result<keyring_core::Entry, SecretError> {
        // iOS：token 只能留在這支手機。預設的存取政策（WhenUnlocked）會跟著加密備份還原到別台裝置；
        // 改成 WhenUnlockedThisDeviceOnly：保留「裝置鎖定時讀不到」的保護，再加上不離開這支裝置。
        // 不選 AfterFirstUnlock：iOS 鎖定後 App 會被暫停，沒有背景讀取的需要；鎖定時寫入失敗由
        // `RetryingStore` 保留並重試。
        #[cfg(target_os = "ios")]
        let entry = keyring_core::Entry::new_with_modifiers(
            SERVICE,
            user,
            &std::collections::HashMap::from([("access-policy", "when-unlocked-this-device-only")]),
        );
        #[cfg(not(target_os = "ios"))]
        let entry = keyring_core::Entry::new(SERVICE, user);
        entry.map_err(|e| SecretError::Store(e.to_string()))
    }

    fn read(user: &str) -> Result<Option<TokenSet>, SecretError> {
        match Self::entry_for(user)?.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| SecretError::Corrupt(e.to_string())),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Store(e.to_string())),
        }
    }

    fn remove(user: &str) -> Result<(), SecretError> {
        match Self::entry_for(user)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Store(e.to_string())),
        }
    }
}

impl SecretStore for KeyringStore {
    /// 先讀自己 client ID 的那一份；沒有才讀舊格式的 key（升級前存的，使用者不必重新登入）。
    /// 舊 key 讀到的若其實是別的 client ID 發的，續期時 GitHub 會回 `incorrect_client_credentials`，
    /// 帳號會要求重新登入，不會動到那顆 token。
    fn get(&self, account_id: u64) -> Result<Option<TokenSet>, SecretError> {
        if let Some(t) = Self::read(&user(self.client_id, account_id))? {
            return Ok(Some(t));
        }
        if self.client_id.is_some() {
            return Self::read(&legacy_user(account_id));
        }
        Ok(None)
    }

    fn put(&self, account_id: u64, t: &TokenSet) -> Result<(), SecretError> {
        let json = serde_json::to_string(t).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        Self::entry_for(&user(self.client_id, account_id))?
            .set_password(&json)
            .map_err(|e| SecretError::Store(e.to_string()))
    }

    /// 本來就沒有也算成功。連舊 key 一起刪：只刪自己那份的話，下一次 `get` 會從舊 key 把舊 token 讀回來。
    fn delete(&self, account_id: u64) -> Result<(), SecretError> {
        Self::remove(&user(self.client_id, account_id))?;
        if self.client_id.is_some() {
            Self::remove(&legacy_user(account_id))?;
        }
        Ok(())
    }
}

/// 寫入失敗時把 token 留在記憶體、之後重試的包裝。
///
/// token 續期成功時 GitHub 會讓舊的 access／refresh token 同時失效；如果這時金鑰庫剛好暫時寫不進去，
/// 新的一組 token 只剩記憶體裡這一份。直接丟掉的話，下一輪會從金鑰庫讀到已經失效的舊 refresh token，
/// 把帳號誤判成要重新登入。所以失敗的寫入留在 `pending`，`get` 優先回傳它並順便重試寫入。
pub struct RetryingStore<S: SecretStore> {
    inner: S,
    pending: Mutex<HashMap<u64, TokenSet>>,
}

impl<S: SecretStore> RetryingStore<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// 還沒寫進底層金鑰庫的帳號數（診斷用）。
    pub fn pending_len(&self) -> usize {
        self.pending.lock().expect("lock").len()
    }
}

impl<S: SecretStore> SecretStore for RetryingStore<S> {
    fn get(&self, account_id: u64) -> Result<Option<TokenSet>, SecretError> {
        let mut pending = self.pending.lock().expect("lock");
        if let Some(t) = pending.get(&account_id).cloned() {
            if self.inner.put(account_id, &t).is_ok() {
                pending.remove(&account_id);
            }
            return Ok(Some(t));
        }
        drop(pending);
        self.inner.get(account_id)
    }

    fn put(&self, account_id: u64, t: &TokenSet) -> Result<(), SecretError> {
        let mut pending = self.pending.lock().expect("lock");
        match self.inner.put(account_id, t) {
            Ok(()) => {
                pending.remove(&account_id);
                Ok(())
            }
            Err(e) => {
                pending.insert(account_id, t.clone());
                Err(e)
            }
        }
    }

    fn delete(&self, account_id: u64) -> Result<(), SecretError> {
        self.pending.lock().expect("lock").remove(&account_id);
        self.inner.delete(account_id)
    }
}

/// macOS：把 Keychain 設為 `keyring-core` 的 default store。App 啟動時呼叫一次。
#[cfg(target_os = "macos")]
pub fn init_native_store() -> Result<(), SecretError> {
    let store = apple_native_keyring_store::keychain::Store::new()
        .map_err(|e| SecretError::Store(e.to_string()))?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// iOS：把 Apple Protected Data（Data Protection 金鑰庫）設為 `keyring-core` 的 default store。App 啟動時呼叫一次。
/// 需要 App 以 provisioning profile 簽章（Xcode 簽章的模擬器／實機 build 都有）；沒簽章會得到 `-34018`。
/// 不同步到 iCloud；每筆 token 另外用 `when-unlocked-this-device-only` 存取政策（見 `KeyringStore::entry`），
/// 所以不會隨加密備份還原到別台裝置。沿用 App 預設的 access group。
#[cfg(target_os = "ios")]
pub fn init_native_store() -> Result<(), SecretError> {
    let store = apple_native_keyring_store::protected::Store::new()
        .map_err(|e| SecretError::Store(e.to_string()))?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// Windows：把認證管理員（Credential Manager）設為 `keyring-core` 的 default store。App 啟動時呼叫一次。
/// 注意：它不保證不同執行緒對同一筆紀錄的操作順序；這裡的寫入（登入、續期）與讀取（下一輪輪詢）相隔數秒。
#[cfg(target_os = "windows")]
pub fn init_native_store() -> Result<(), SecretError> {
    let store = windows_native_keyring_store::Store::new()
        .map_err(|e| SecretError::Store(e.to_string()))?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// Android：把 Android Keystore＋SharedPreferences 的 store 設為 `keyring-core` 的 default store。
/// 必須在 Tauri 已經提供 application context（`ndk-context`）之後才能呼叫（Tauri Mobile 會在啟動時初始化它），
/// 所以放在 `setup` 裡，不要更早。
#[cfg(target_os = "android")]
pub fn init_native_store() -> Result<(), SecretError> {
    let store = android_native_keyring_store::Store::new()
        .map_err(|e| SecretError::Store(e.to_string()))?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// 測試用：放在記憶體裡（同樣以 JSON 存，行為與金鑰庫一致）。
#[derive(Debug, Default)]
pub struct MemoryStore {
    items: Mutex<HashMap<u64, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account_id: u64) -> Result<Option<TokenSet>, SecretError> {
        match self.items.lock().expect("lock").get(&account_id) {
            Some(json) => serde_json::from_str(json)
                .map(Some)
                .map_err(|e| SecretError::Corrupt(e.to_string())),
            None => Ok(None),
        }
    }

    fn put(&self, account_id: u64, t: &TokenSet) -> Result<(), SecretError> {
        let json = serde_json::to_string(t).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        self.items.lock().expect("lock").insert(account_id, json);
        Ok(())
    }

    fn delete(&self, account_id: u64) -> Result<(), SecretError> {
        self.items.lock().expect("lock").remove(&account_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> TokenSet {
        TokenSet {
            access_token: "ghu_a".into(),
            expires_at: Some(100),
            refresh_token: Some("ghr_b".into()),
            refresh_expires_at: Some(200),
        }
    }

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::new();
        assert_eq!(s.get(1).unwrap(), None);
        s.put(1, &token()).unwrap();
        assert_eq!(s.get(1).unwrap(), Some(token()));
        assert_eq!(s.get(2).unwrap(), None, "帳號之間互不影響");
        s.delete(1).unwrap();
        assert_eq!(s.get(1).unwrap(), None);
        s.delete(1).unwrap(); // 刪兩次不報錯
    }

    /// 前 N 次 put 失敗的假金鑰庫
    struct Flaky {
        inner: MemoryStore,
        failures_left: Mutex<u32>,
    }
    impl SecretStore for Flaky {
        fn get(&self, id: u64) -> Result<Option<TokenSet>, SecretError> {
            self.inner.get(id)
        }
        fn put(&self, id: u64, t: &TokenSet) -> Result<(), SecretError> {
            let mut n = self.failures_left.lock().unwrap();
            if *n > 0 {
                *n -= 1;
                return Err(SecretError::Store("locked".into()));
            }
            self.inner.put(id, t)
        }
        fn delete(&self, id: u64) -> Result<(), SecretError> {
            self.inner.delete(id)
        }
    }

    #[test]
    fn retrying_store_keeps_a_failed_write_and_retries_it() {
        let old = token();
        let new = TokenSet {
            access_token: "ghu_new".into(),
            refresh_token: Some("ghr_new".into()),
            ..token()
        };
        let store = RetryingStore::new(Flaky {
            inner: MemoryStore::new(),
            failures_left: Mutex::new(0),
        });
        store.put(1, &old).unwrap();

        *store.inner.failures_left.lock().unwrap() = 1;
        assert!(store.put(1, &new).is_err(), "寫入失敗要讓呼叫端知道");
        assert_eq!(store.pending_len(), 1);
        // 底層還是舊的那組，但 get 必須回新的一組（舊的 refresh token 已經失效）；順便重試寫入
        assert_eq!(store.get(1).unwrap(), Some(new.clone()));
        assert_eq!(store.pending_len(), 0, "重試成功後不再留著");
        assert_eq!(store.inner.inner.get(1).unwrap(), Some(new));
    }

    #[test]
    fn retrying_store_keeps_pending_while_the_store_keeps_failing_and_delete_clears_it() {
        let new = token();
        let store = RetryingStore::new(Flaky {
            inner: MemoryStore::new(),
            failures_left: Mutex::new(5),
        });
        assert!(store.put(1, &new).is_err());
        assert_eq!(store.get(1).unwrap(), Some(new.clone()));
        assert_eq!(
            store.get(1).unwrap(),
            Some(new),
            "仍然寫不進去：照樣從記憶體回"
        );
        assert_eq!(store.pending_len(), 1);
        store.delete(1).unwrap();
        assert_eq!(store.pending_len(), 0);
        assert_eq!(store.get(1).unwrap(), None, "刪除後不會又冒出來");
    }

    #[test]
    fn put_replaces_the_old_token_pair() {
        let s = MemoryStore::new();
        s.put(1, &token()).unwrap();
        let newer = TokenSet {
            access_token: "ghu_new".into(),
            ..token()
        };
        s.put(1, &newer).unwrap();
        assert_eq!(s.get(1).unwrap(), Some(newer));
    }

    /// 真的寫入 macOS Keychain 再刪掉：`cargo test -p pr-runners-core keyring_store_roundtrip -- --ignored`
    #[test]
    #[ignore = "會動到真的 Keychain，只在 Mac 手動跑"]
    #[cfg(target_os = "macos")]
    fn keyring_store_roundtrip() {
        init_native_store().unwrap();
        let s = KeyringStore::new();
        let id = 999_000_001; // 不會是真實帳號
        s.delete(id).unwrap();
        assert_eq!(s.get(id).unwrap(), None);
        s.put(id, &token()).unwrap();
        assert_eq!(s.get(id).unwrap(), Some(token()));
        s.delete(id).unwrap();
        assert_eq!(s.get(id).unwrap(), None);
    }

    /// `keyring_core` 的 default store 是行程全域的：用到它的測試要排隊，不然會互相換掉對方的 store。
    static KEYRING_LOCK: Mutex<()> = Mutex::new(());

    fn fresh_mock_store() -> std::sync::MutexGuard<'static, ()> {
        let guard = KEYRING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        guard
    }

    #[test]
    fn keyring_store_tokens_are_scoped_by_client_id() {
        let _g = fresh_mock_store();
        let prod = KeyringStore::with_client_id(Some("Iv_prod"));
        let dev = KeyringStore::with_client_id(Some("Iv_dev"));
        let prod_token = token();
        let dev_token = TokenSet {
            access_token: "ghu_dev".into(),
            ..token()
        };
        prod.put(5, &prod_token).unwrap();
        assert_eq!(dev.get(5).unwrap(), None, "dev 版看不到正式版的 token");
        dev.put(5, &dev_token).unwrap();
        assert_eq!(
            prod.get(5).unwrap(),
            Some(prod_token),
            "dev 版登入不能蓋掉正式版的 token"
        );
        assert_eq!(dev.get(5).unwrap(), Some(dev_token));
        prod.delete(5).unwrap();
        assert!(dev.get(5).unwrap().is_some(), "刪除只動自己那一份");
    }

    #[test]
    fn keyring_store_reads_legacy_key_and_prefers_its_own() {
        let _g = fresh_mock_store();
        let legacy = keyring_core::Entry::new(SERVICE, "github:6").unwrap();
        legacy
            .set_password(&serde_json::to_string(&token()).unwrap())
            .unwrap();

        let s = KeyringStore::with_client_id(Some("Iv_prod"));
        assert_eq!(
            s.get(6).unwrap(),
            Some(token()),
            "升級前存的 token（沒有 client ID 的舊 key）要讀得到，使用者不必重新登入"
        );
        let newer = TokenSet {
            access_token: "ghu_new".into(),
            ..token()
        };
        s.put(6, &newer).unwrap();
        assert_eq!(s.get(6).unwrap(), Some(newer), "寫入之後以自己的 key 為準");

        s.delete(6).unwrap();
        assert_eq!(
            s.get(6).unwrap(),
            None,
            "刪除要連舊 key 一起清掉，不然會冒出舊 token"
        );
        assert!(matches!(
            legacy.get_password(),
            Err(keyring_core::Error::NoEntry)
        ));
    }

    #[test]
    fn keyring_store_without_client_id_keeps_using_the_plain_key() {
        let _g = fresh_mock_store();
        let s = KeyringStore::with_client_id(None);
        s.put(8, &token()).unwrap();
        let plain = keyring_core::Entry::new(SERVICE, "github:8").unwrap();
        assert!(
            plain.get_password().is_ok(),
            "沒設 client ID 的建置（只用 PAT）照舊"
        );
    }

    #[test]
    fn keyring_store_with_keyring_core_mock_store() {
        let _g = fresh_mock_store();
        let s = KeyringStore::new();
        assert_eq!(s.get(7).unwrap(), None);
        s.put(7, &token()).unwrap();
        assert_eq!(s.get(7).unwrap(), Some(token()));
        s.delete(7).unwrap();
        s.delete(7).unwrap();
        assert_eq!(s.get(7).unwrap(), None);
    }
}
