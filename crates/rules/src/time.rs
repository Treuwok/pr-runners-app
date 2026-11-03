//! 時間可信度：資料可疑就標 `Untrusted`，不顯示看起來精確、其實是錯的數字。

use crate::model::Elapsed;

/// 已跑時間。開始時間在未來，或比建立時間還早，都不可信。
pub fn elapsed(now: i64, created_at: i64, started_at: i64) -> Elapsed {
    if started_at > now || started_at < created_at {
        Elapsed::Untrusted
    } else {
        Elapsed::Known {
            secs: now - started_at,
        }
    }
}

/// 排了多久。`created_at` 在未來（GitHub 時間戳偏移）就不可信。
///
/// 不可信時不顯示數字；但「有沒有超過 10 分鐘」仍要判斷，那時改用本機第一次看見
/// 這個 job 的時間（見 `attention::evaluate`）。
pub fn queued_age(now: i64, created_at: i64) -> Elapsed {
    if created_at > now {
        Elapsed::Untrusted
    } else {
        Elapsed::Known {
            secs: now - created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_job_elapsed_is_now_minus_start() {
        assert_eq!(elapsed(1_000, 100, 400), Elapsed::Known { secs: 600 });
    }

    #[test]
    fn start_in_future_is_untrusted() {
        // 2026-11-01 的 +1h 偏移
        assert_eq!(elapsed(1_000, 900, 4_000), Elapsed::Untrusted);
    }

    #[test]
    fn start_before_creation_is_untrusted() {
        assert_eq!(elapsed(1_000, 500, 400), Elapsed::Untrusted);
    }

    #[test]
    fn start_exactly_now_is_zero() {
        assert_eq!(elapsed(1_000, 900, 1_000), Elapsed::Known { secs: 0 });
    }

    #[test]
    fn queued_age_uses_created_at_when_plausible() {
        assert_eq!(queued_age(1_000, 300), Elapsed::Known { secs: 700 });
    }

    #[test]
    fn queued_age_is_untrusted_when_created_in_future() {
        // created_at 在未來 → 不可信，讓 UI 顯示 ⚠?（門檻判斷改用首見時間，測試在 attention）
        assert_eq!(queued_age(1_000, 4_600), Elapsed::Untrusted);
    }
}
