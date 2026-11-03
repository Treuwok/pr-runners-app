//! runner 監控的核心：I/O 與組裝。不依賴 Tauri；純判斷規則在 `pr-runners-rules`。

pub mod api;
pub mod assemble;
pub mod auth;
pub mod client;
pub mod collector;
pub mod history;
pub mod persist;
pub mod runlog;
pub mod secrets;
pub mod view;

pub use client::now_secs;
pub use client::{ApiError, GitHubClient, RateState};
pub use collector::{Collector, Hold, TickMode};
