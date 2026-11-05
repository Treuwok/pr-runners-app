//! 純規則：不依賴 tokio／reqwest／任何 I/O，必須能編成 wasm32（日後給 Worker 重用）。
//! 時間一律是 `i64` Unix 秒（UTC）；ISO 8601 的解析只在 core 做。

pub mod attention;
pub mod fleet;
pub mod matching;
pub mod model;
pub mod time;

pub use attention::{
    Attention, AttentionEvent, JobObs, OFFLINE_GRACE_SECS, ObservationState, OfflineSince,
    QUEUE_LIMIT_SECS, RunnerObs, STALE_GAP_SECS, evaluate,
};
pub use fleet::{Fleet, PoolTag, pool_tag};
pub use matching::{
    Candidate, CandidateKind, candidates, is_blacksmith, is_blacksmith_machine,
    is_blacksmith_runner, needs_self_hosted,
};
pub use model::{Elapsed, GroupAccess, JobFacts, JobState, RunnerFacts};
pub use time::{elapsed, queued_age};
