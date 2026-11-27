//! 診斷 log：把「帳號為什麼被標成離線」的真正原因寫進 App 資料夾的 `diagnostic.log`。
//!
//! GUI App 的 stderr 看不到，而 `Offline` 狀態把續期失敗、金鑰庫讀取失敗、掃描時各種 API 錯誤都併成一種，
//! 事後無從區分。這裡只記錄錯誤的種類與訊息（GitHub 回的 `error` 欄位、HTTP 狀態、網路錯誤文字），
//! 不記 token、不記回應本文。寫檔失敗一律吞掉：診斷不能影響輪詢。

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

const FILE_NAME: &str = "diagnostic.log";
/// 超過就只留後半段，避免無限長大（每 2 分鐘最多幾行，正常用量遠低於這個數字）。
const MAX_BYTES: u64 = 128 * 1024;

static PATH: OnceLock<PathBuf> = OnceLock::new();

/// App 啟動時呼叫一次；沒呼叫的話 `log` 什麼都不做（測試、CLI）。
pub fn init(data_dir: &std::path::Path) {
    let _ = PATH.set(data_dir.join(FILE_NAME));
}

/// 追加一行：`<本地時間> [<分類>] <訊息>`。
pub fn log(kind: &str, msg: &str) {
    let Some(path) = PATH.get() else { return };
    let _ = append(path, &format_line(crate::now_secs(), kind, msg));
}

fn format_line(now: i64, kind: &str, msg: &str) -> String {
    // 單行：訊息裡的換行換成空白，避免一筆錯誤吃掉多行
    let msg = msg.replace(['\r', '\n'], " ");
    format!("{now} [{kind}] {msg}\n")
}

fn append(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let old = std::fs::read_to_string(path).unwrap_or_default();
        let keep = old.get(old.len() / 2..).unwrap_or("");
        // 從下一個完整行開始留
        let keep = keep.split_once('\n').map_or("", |(_, rest)| rest);
        std::fs::write(path, keep)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_is_single_line_with_kind() {
        let l = format_line(1, "refresh", "a\nb\r\nc");
        assert_eq!(l, "1 [refresh] a b  c\n");
    }

    #[test]
    fn append_truncates_to_recent_half_when_too_big() {
        let dir = std::env::temp_dir().join(format!("prr-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.log");
        let big = "x".repeat(100);
        let mut s = String::new();
        let mut i = 0;
        while (s.len() as u64) <= MAX_BYTES {
            s.push_str(&format!("{i} {big}\n"));
            i += 1;
        }
        std::fs::write(&p, &s).unwrap();
        append(&p, "last\n").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.len() < s.len());
        assert!(out.ends_with("last\n"));
        assert!(out.lines().all(|l| l == "last" || l.ends_with(&big)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
