//! run → 畫面上的 PR 標籤（`PR#12`、`push:main←#12`…）。需要反查 commit 時查 GitHub，結果依 commit 快取。

use crate::api::RunInfo;
use crate::client::{ApiError, GitHubClient};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

#[derive(Deserialize)]
struct RawPull {
    number: u64,
    state: String,
}

#[derive(Debug, Clone, PartialEq)]
struct PullRef {
    number: u64,
    open: bool,
}

/// commit → PR 查詢結果，以 `(repo, head_sha)` 快取，同一個 sha 不重查。
/// 只快取查詢結果；最終標籤仍依每個 run 的 event／branch／pr_number 組成，
/// 避免同 SHA 的 push 與 pull_request run 共用錯誤標籤。
/// 同一個 repo 的 run 是並行查的（一次 push 常觸發好幾個 workflow，sha 都一樣）：
/// 同一個 key 同時只送一個請求，其他人等它的結果。查詢失敗不留結果，下次再查。
#[derive(Default)]
pub struct PrTagCache {
    pulls: Mutex<HashMap<(String, String), PullLookup>>,
}

/// 一個 `(repo, head_sha)` 的查詢：第一個來的人負責查，同時來的人等同一個結果。
type PullLookup = Arc<OnceCell<Option<PullRef>>>;

/// 長時間執行的 App 不能讓這張表無限長大；滿了就清空（重查只花一個請求）。
const MAX_PR_CACHE_ENTRIES: usize = 5_000;

impl PrTagCache {
    async fn first_pull(
        &self,
        c: &GitHubClient,
        run: &RunInfo,
    ) -> Result<Option<PullRef>, ApiError> {
        let cell = {
            let mut cache = self.pulls.lock().expect("pr cache");
            if cache.len() >= MAX_PR_CACHE_ENTRIES {
                cache.clear();
            }
            cache
                .entry((run.repo.clone(), run.head_sha.clone()))
                .or_default()
                .clone()
        };
        cell.get_or_try_init(|| async {
            let pulls: Vec<RawPull> = c
                .get_json(&format!(
                    "/repos/{}/commits/{}/pulls",
                    run.repo, run.head_sha
                ))
                .await?;
            Ok(pulls.into_iter().next().map(|p| PullRef {
                number: p.number,
                open: p.state == "open",
            }))
        })
        .await
        .cloned()
    }
}

/// 規則照搬 `bin/pr-runners` 的 `_scan_repo`：
/// - run 本身帶 PR 號 → `PR#N`
/// - push 事件 → `push:{branch}`，反查到來源 PR 再補 `←#N`
/// - 其他 → 反查 commit：open → `PR#N`、非 open → `PR#N(closed)`，查不到 → `{event}:{branch}`
///
/// 只在真的要顯示時才呼叫（沒有 self-hosted job 的 run 不必花這個請求）。
/// 不查 API 就能組出的標籤：`PR#N`，或 `{event}:{branch}`（沒有 branch 就只有 event）。
pub fn local_tag(run: &RunInfo) -> String {
    if let Some(n) = run.pr_number {
        return format!("PR#{n}");
    }
    with_branch(&run.event, run.head_branch.as_deref())
}

fn with_branch(prefix: &str, branch: Option<&str>) -> String {
    match branch {
        Some(b) if !b.is_empty() => format!("{prefix}:{b}"),
        _ => prefix.to_string(),
    }
}

pub async fn pr_tag(
    c: &GitHubClient,
    cache: &PrTagCache,
    run: &RunInfo,
) -> Result<String, ApiError> {
    if run.pr_number.is_some() {
        return Ok(local_tag(run));
    }
    let found = cache.first_pull(c, run).await?;
    Ok(if run.event == "push" {
        let push = with_branch("push", run.head_branch.as_deref());
        match found {
            Some(p) => format!("{push}←#{}", p.number),
            None => push,
        }
    } else {
        match found {
            Some(p) if p.open => format!("PR#{}", p.number),
            Some(p) => format!("PR#{}(closed)", p.number),
            None => local_tag(run),
        }
    })
}
