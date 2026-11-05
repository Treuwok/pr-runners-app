//! 本機保存：觀察狀態、最後一份 snapshot、帳號清單、設定、runner 執行歷史。
//! 全部放在呼叫端給的資料夾（App 的 app_data_dir），寫入一律「寫暫存檔再 rename」。
//! token 不在這裡——只放系統金鑰庫（見 secrets）。

use crate::runlog::RunLog;
use crate::view::{SCHEMA_VERSION, Snapshot};
use pr_runners_rules::ObservationState;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::RangeInclusive;
use std::path::Path;

/// 每個檔案各自的版本。帳號清單與設定是使用者的設定，不能因為 Snapshot 形狀改版
/// （`view::SCHEMA_VERSION`）就被當成版本不符而丟掉，讓使用者默默被登出。
const ACCOUNTS_SCHEMA: u32 = 1;
const SETTINGS_SCHEMA: u32 = 1;
const RUNLOG_SCHEMA: u32 = 1;

const OBSERVATIONS: &str = "observations.json";
const SNAPSHOT: &str = "last_snapshot.json";
const ACCOUNTS: &str = "accounts.json";
const SETTINGS: &str = "settings.json";
const RUNLOG: &str = "runlog.json";

/// 每小時請求目標：預設值與可調範圍（spec §7）。
pub const DEFAULT_HOURLY_TARGET: u32 = 2_500;
pub const HOURLY_TARGET_RANGE: RangeInclusive<u32> = 500..=4_500;

pub fn clamp_hourly_target(n: u32) -> u32 {
    n.clamp(*HOURLY_TARGET_RANGE.start(), *HOURLY_TARGET_RANGE.end())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountKind {
    GitHubApp,
    Pat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountMeta {
    pub account_id: u64,
    pub login: String,
    pub kind: AccountKind,
    /// 使用者勾選要看的 org／個人帳號（scope 名稱）
    pub selected_scopes: BTreeSet<String>,
}

/// 主視窗背景不透明度（百分比）的下限：再低就看不清楚了。
pub const MIN_WINDOW_OPACITY: u8 = 40;

pub fn clamp_window_opacity(n: u8) -> u8 {
    n.clamp(MIN_WINDOW_OPACITY, 100)
}

fn full_opacity() -> u8 {
    100
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct Settings {
    pub schema_version: u32,
    pub hourly_request_target: u32,
    /// 主視窗永遠在最上層（只有桌面版）。舊的 settings.json 沒有這欄 → false。
    #[serde(default)]
    pub always_on_top: bool,
    /// 主視窗背景不透明度 40–100（%），文字不受影響。舊的 settings.json 沒有這欄 → 100。
    #[serde(default = "full_opacity")]
    pub window_opacity: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA,
            hourly_request_target: DEFAULT_HOURLY_TARGET,
            always_on_top: false,
            window_opacity: full_opacity(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Versioned<T> {
    schema_version: u32,
    #[serde(flatten)]
    body: T,
}

#[derive(Serialize, Deserialize)]
struct StatesBody {
    states: BTreeMap<String, ObservationState>,
}

#[derive(Serialize, Deserialize)]
struct AccountsBody {
    accounts: Vec<AccountMeta>,
}

fn write_atomic(dir: &Path, name: &str, json: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, dir.join(name))
}

fn read_json<T: DeserializeOwned>(dir: &Path, name: &str) -> Option<T> {
    serde_json::from_slice(&std::fs::read(dir.join(name)).ok()?).ok()
}

fn save<T: Serialize>(dir: &Path, name: &str, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(dir, name, &json)
}

/// 版本不符或壞掉都丟棄重來（觀察狀態只是輔助，不值得擋住啟動）。
pub fn load_state(dir: &Path) -> BTreeMap<String, ObservationState> {
    read_json::<Versioned<StatesBody>>(dir, OBSERVATIONS)
        .filter(|v| v.schema_version == SCHEMA_VERSION)
        .map(|v| v.body.states)
        .unwrap_or_default()
}

pub fn save_state(dir: &Path, states: &BTreeMap<String, ObservationState>) -> io::Result<()> {
    save(
        dir,
        OBSERVATIONS,
        &Versioned {
            schema_version: SCHEMA_VERSION,
            body: StatesBody {
                states: states.clone(),
            },
        },
    )
}

#[derive(Serialize, Deserialize)]
struct RunLogBody {
    runlog: RunLog,
}

/// runner 執行歷史；沒有、壞掉或版本不符 → 空的（之後重新累積，不影響其他功能）。
pub fn load_runlog(dir: &Path) -> RunLog {
    read_json::<Versioned<RunLogBody>>(dir, RUNLOG)
        .filter(|v| v.schema_version == RUNLOG_SCHEMA)
        .map(|v| v.body.runlog)
        .unwrap_or_default()
}

pub fn save_runlog(dir: &Path, runlog: &RunLog) -> io::Result<()> {
    save(
        dir,
        RUNLOG,
        &Versioned {
            schema_version: RUNLOG_SCHEMA,
            body: RunLogBody {
                runlog: runlog.clone(),
            },
        },
    )
}

pub fn load_snapshot(dir: &Path) -> Option<Snapshot> {
    read_json::<Snapshot>(dir, SNAPSHOT).filter(|s| s.schema_version == SCHEMA_VERSION)
}

pub fn save_snapshot(dir: &Path, snapshot: &Snapshot) -> io::Result<()> {
    save(dir, SNAPSHOT, snapshot)
}

pub fn load_accounts(dir: &Path) -> Vec<AccountMeta> {
    read_json::<Versioned<AccountsBody>>(dir, ACCOUNTS)
        .filter(|v| v.schema_version == ACCOUNTS_SCHEMA)
        .map(|v| v.body.accounts)
        .unwrap_or_default()
}

pub fn save_accounts(dir: &Path, accounts: &[AccountMeta]) -> io::Result<()> {
    save(
        dir,
        ACCOUNTS,
        &Versioned {
            schema_version: ACCOUNTS_SCHEMA,
            body: AccountsBody {
                accounts: accounts.to_vec(),
            },
        },
    )
}

/// 檔案不存在、壞掉或版本不符 → 預設值；讀到的值也經 clamp。
pub fn load_settings(dir: &Path) -> Settings {
    match read_json::<Settings>(dir, SETTINGS).filter(|s| s.schema_version == SETTINGS_SCHEMA) {
        Some(s) => Settings {
            hourly_request_target: clamp_hourly_target(s.hourly_request_target),
            window_opacity: clamp_window_opacity(s.window_opacity),
            ..s
        },
        None => Settings::default(),
    }
}

pub fn save_settings(dir: &Path, settings: &Settings) -> io::Result<()> {
    save(dir, SETTINGS, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Summary;
    use pr_runners_rules::OfflineSince;

    #[test]
    fn runlog_roundtrip_and_broken_file_is_empty() {
        use crate::runlog::{JobOutcome, JobRecord, RecordSource};
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_runlog(dir.path()), RunLog::default(), "沒有檔案");
        let mut log = RunLog::default();
        log.insert(
            "1/A/9",
            1,
            JobRecord {
                job_id: 5,
                run_id: 4,
                repo: "A/r".into(),
                workflow: "ci".into(),
                job: "build".into(),
                pr_tag: "PR#1".into(),
                url: String::new(),
                started_at: Some(10),
                completed_at: Some(20),
                outcome: JobOutcome::Success,
                source: RecordSource::Observed,
                checked_at: 20,
            },
        );
        save_runlog(dir.path(), &log).unwrap();
        assert_eq!(load_runlog(dir.path()), log);
        std::fs::write(dir.path().join(RUNLOG), b"{not json").unwrap();
        assert_eq!(
            load_runlog(dir.path()),
            RunLog::default(),
            "壞掉的檔案不讓 App 起不來"
        );
    }

    #[test]
    fn window_prefs_default_for_old_files_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        // 這個版本以前寫出的 settings.json 沒有視窗欄位
        std::fs::write(
            dir.path().join(SETTINGS),
            format!(r#"{{"schema_version":{SETTINGS_SCHEMA},"hourly_request_target":3000}}"#),
        )
        .unwrap();
        let s = load_settings(dir.path());
        assert_eq!(s.hourly_request_target, 3_000, "舊檔的其他設定照讀");
        assert!(!s.always_on_top);
        assert_eq!(s.window_opacity, 100);

        save_settings(
            dir.path(),
            &Settings {
                always_on_top: true,
                window_opacity: 5,
                ..s
            },
        )
        .unwrap();
        let s = load_settings(dir.path());
        assert!(s.always_on_top);
        assert_eq!(s.window_opacity, MIN_WINDOW_OPACITY, "讀回時也經 clamp");
        assert_eq!(clamp_window_opacity(255), 100);
        assert_eq!(clamp_window_opacity(70), 70);
    }

    #[test]
    fn settings_default_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_settings(dir.path()).hourly_request_target,
            2_500,
            "沒有 settings.json"
        );
        assert_eq!(clamp_hourly_target(100), 500);
        assert_eq!(clamp_hourly_target(9_999), 4_500);
        assert_eq!(clamp_hourly_target(2_000), 2_000);
        save_settings(
            dir.path(),
            &Settings {
                hourly_request_target: 3_000,
                ..Settings::default()
            },
        )
        .unwrap();
        assert_eq!(load_settings(dir.path()).hourly_request_target, 3_000);
    }

    #[test]
    fn settings_out_of_range_or_broken_are_repaired() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"schema_version":1,"hourly_request_target":99999}"#,
        )
        .unwrap();
        assert_eq!(
            load_settings(dir.path()).hourly_request_target,
            4_500,
            "讀到的值也經 clamp"
        );
        std::fs::write(dir.path().join("settings.json"), "{not json").unwrap();
        assert_eq!(load_settings(dir.path()), Settings::default());
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"schema_version":99,"hourly_request_target":3000}"#,
        )
        .unwrap();
        assert_eq!(load_settings(dir.path()), Settings::default(), "版本不符");
    }

    #[test]
    fn state_roundtrip_and_version_mismatch_discards() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_state(dir.path()).is_empty());
        let mut st = ObservationState::default();
        st.offline_since
            .insert("1/o/5".into(), OfflineSince::Observed(42));
        st.last_observed_at = Some(50);
        let states = BTreeMap::from([("1/o".to_string(), st)]);
        save_state(dir.path(), &states).unwrap();
        assert_eq!(load_state(dir.path()), states);
        std::fs::write(
            dir.path().join("observations.json"),
            r#"{"schema_version":2,"states":{}}"#,
        )
        .unwrap();
        assert!(load_state(dir.path()).is_empty());
    }

    #[test]
    fn snapshot_roundtrip_and_version_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_snapshot(dir.path()).is_none());
        let snap = Snapshot {
            schema_version: SCHEMA_VERSION,
            generated_at: 7,
            accounts: Vec::new(),
            summary: Summary {
                own_busy: 1,
                own_online: 2,
                own_offline: 3,
                pool_busy: 5,
                waiting: 4,
                attention: crate::view::Severity::Warn,
                stale: false,
            },
        };
        save_snapshot(dir.path(), &snap).unwrap();
        assert_eq!(load_snapshot(dir.path()), Some(snap.clone()));
        let old = Snapshot {
            schema_version: 0,
            ..snap
        };
        save_snapshot(dir.path(), &old).unwrap();
        assert!(load_snapshot(dir.path()).is_none());
    }

    #[test]
    fn accounts_roundtrip_and_no_temp_file_left() {
        let dir = tempfile::tempdir().unwrap();
        let accts = vec![AccountMeta {
            account_id: 9,
            login: "x".into(),
            kind: AccountKind::Pat,
            selected_scopes: BTreeSet::from(["x".to_string()]),
        }];
        save_accounts(dir.path(), &accts).unwrap();
        assert_eq!(load_accounts(dir.path()), accts);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["accounts.json"], "寫暫存檔再 rename，不留暫存檔");
    }
}
