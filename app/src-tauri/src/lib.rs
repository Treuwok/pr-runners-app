mod commands;
mod poller;
mod state;
mod tray;
// Task 8 才接上選單列；在那之前只有測試用到
#[allow(dead_code)]
mod tray_icon;

use pr_runners_core::auth::{CLIENT_ID, DeviceFlow};
use pr_runners_core::persist::{
    AccountMeta, load_accounts, load_runlog, load_settings, load_state,
};
use pr_runners_core::secrets::{KeyringStore, RetryingStore, SecretStore};
use pr_runners_core::{Collector, GitHubClient, Hold, now_secs};
use state::Shared;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use tauri::Manager;
use tokio::sync::Notify;
use url::Url;

/// 啟動時把已存的帳號接上金鑰庫裡的 token，建出收集器。
/// 金鑰庫裡沒有 token 的帳號直接標「重新登入」。
fn build_collector(
    data_dir: &std::path::Path,
    api_base: &Url,
    secrets: &dyn SecretStore,
    halt: &Arc<AtomicBool>,
) -> Collector {
    let metas: Vec<AccountMeta> = load_accounts(data_dir);
    let mut missing = Vec::new();
    let accounts: Vec<(AccountMeta, GitHubClient)> = metas
        .into_iter()
        .map(|meta| {
            let token = match secrets.get(meta.account_id) {
                Ok(Some(t)) => t.access_token,
                _ => {
                    missing.push(meta.account_id);
                    String::new()
                }
            };
            (
                meta,
                GitHubClient::new(api_base.clone(), token).with_halt(halt.clone()),
            )
        })
        .collect();
    let settings = load_settings(data_dir);
    let mut collector = Collector::new(
        accounts,
        load_state(data_dir),
        settings.hourly_request_target,
        now_secs,
    );
    for id in missing {
        collector.hold(id, Some(Hold::NeedsReauth));
    }
    collector.set_runlog(load_runlog(data_dir));
    collector
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init());
    // 主視窗記住上次的大小（只記大小）；小視窗的大小改由設定檔的 popover_size 管、位置永遠跟著圖示
    #[cfg(desktop)]
    let builder = builder.plugin(
        tauri_plugin_window_state::Builder::default()
            .with_state_flags(tauri_plugin_window_state::StateFlags::SIZE)
            .with_denylist(&["popover"])
            .build(),
    );

    builder
        .setup(|app| {
            // macOS：Keychain；Windows：認證管理員；Android：Keystore＋SharedPreferences（需要 Tauri 已提供 application context，
            // 所以在 setup 裡建立）。兩者都是 keyring-core 的 default store。
            #[cfg(any(target_os = "macos", target_os = "android", target_os = "windows"))]
            pr_runners_core::secrets::init_native_store()
                .map_err(|e| format!("無法使用系統金鑰庫：{e}"))?;

            // 選單列 App：不出現在 Dock
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            #[cfg(desktop)]
            tray::setup(app)?;

            let data_dir = app.path().app_data_dir()?;
            // 上次設定的「主視窗永遠在最上層」
            #[cfg(desktop)]
            tray::apply_always_on_top(app.handle(), load_settings(&data_dir).always_on_top);
            let api_base = Url::parse("https://api.github.com").expect("static url");
            let secrets = RetryingStore::new(KeyringStore::new());
            let halt = Arc::new(AtomicBool::new(false));
            let accounts = load_accounts(&data_dir);
            let collector = build_collector(&data_dir, &api_base, &secrets, &halt);
            let runlog = collector.runlog().clone();

            let shared = Arc::new(Shared {
                data_dir,
                api_base,
                collector: tokio::sync::Mutex::new(collector),
                lifecycle: tokio::sync::Mutex::new(()),
                ops: Mutex::new(Vec::new()),
                accounts: Mutex::new(accounts),
                latest: Mutex::new(None),
                runlog: Mutex::new(runlog),
                runlog_dirty: AtomicBool::new(false),
                secrets,
                flow: CLIENT_ID.map(DeviceFlow::new),
                pending_login: Mutex::new(None),
                wake: Notify::new(),
                pending_token: Mutex::new(None),
                halt,
                force_full: AtomicBool::new(false),
                resume_pending: AtomicBool::new(false),
            });
            app.manage(shared.clone());
            tauri::async_runtime::spawn(poller::run_loop(app.handle().clone(), shared));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::refresh_now,
            commands::login_start,
            commands::login_poll,
            commands::list_scopes,
            commands::set_scopes,
            commands::remove_account,
            commands::get_settings,
            commands::set_hourly_target,
            commands::set_window_prefs,
            commands::install_url,
            commands::add_pat,
            commands::open_url,
            commands::app_resumed,
            commands::app_paused,
            commands::secret_selftest,
            commands::show_main,
            commands::runner_history,
            commands::backfill_runner_history,
            commands::show_runner_history,
            commands::quit_app,
            commands::resize_popover,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
