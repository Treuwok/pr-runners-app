//! macOS 選單列：計數文字＋狀態圖示，左鍵點開小視窗（popover）。
//! 文字與圖示只由 Rust 端算（前端不重寫一份）。

use pr_runners_core::view::{Severity, Summary};

/// 選單列計數：`{busy}▶`，有等待時加 ` {waiting}⏳`；沒有任何 runner 時是空字串（只顯示中性圖示）。
pub fn tray_title(s: &Summary) -> String {
    if !has_runners(s) {
        return String::new();
    }
    let busy = s.own_busy + s.pool_busy;
    if s.waiting > 0 {
        format!("{busy}▶ {}⏳", s.waiting)
    } else {
        format!("{busy}▶")
    }
}

/// 無 runner → neutral；排隊太久是橘。（Task 8 會整個換成依數字畫的小圖。）
pub fn tray_icon_for(sev: Severity, has_runners: bool) -> &'static str {
    if !has_runners {
        return "neutral";
    }
    match sev {
        Severity::Ok => "ok",
        Severity::Warn => "warn",
    }
}

pub fn has_runners(s: &Summary) -> bool {
    s.own_online + s.own_offline + s.pool_busy > 0
}

/// 系統匣圖示的提示文字。Windows 的系統匣不能顯示文字，計數只看得到這裡（macOS 也會顯示）。
pub fn tray_tooltip(s: &Summary) -> String {
    if !has_runners(s) {
        return "pr-runners".into();
    }
    let mut parts = vec![
        format!("{} 忙碌", s.own_busy + s.pool_busy),
        format!("{} 閒置", s.own_online - s.own_busy),
    ];
    if s.own_offline > 0 {
        parts.push(format!("{} 離線", s.own_offline));
    }
    parts.push(format!("{} 排隊", s.waiting));
    format!("pr-runners · {}", parts.join(" · "))
}

/// 一個螢幕：位置與大小用該螢幕自己的縮放比例換成實體像素（macOS／tao 的慣例），`scale` 是那個比例。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
}

const POPOVER_MARGIN: f64 = 8.0;
/// 選單列圖示的高度（點）：一般螢幕 24、有瀏海的 MacBook 約 37。
const MENU_BAR_PT: std::ops::RangeInclusive<f64> = 16.0..=40.0;

/// 小視窗左上角的位置（以圖示所在螢幕的比例換算的邏輯座標），以及用的是哪個比例。
/// 圖示在螢幕上半部（macOS 選單列）→ 開在圖示正下方；在下半部（Windows 工作列）→ 開在圖示正上方。
/// 一律水平置中、不超出圖示所在的螢幕。`tray` 是圖示的 (x, y, w, h)，用「圖示所在螢幕」的比例換成的實體像素。
///
/// 圖示與螢幕的實體座標都是「邏輯座標 × 各自螢幕的比例」（macOS 的慣例；Windows 本身就是實體像素，
/// 這樣換算結果相同）。macOS 上縮放不同的螢幕（Retina 2× 與外接 1×）實體範圍會重疊，光看「哪個螢幕
/// 包含它」可能挑錯，所以另外要求：用該螢幕的比例換回之後，圖示高度要像一條選單列；
/// 用錯比例會得到 12 或 74 點這種不可能的高度。結果不受螢幕列舉順序影響。
pub fn popover_origin(
    tray: (f64, f64, f64, f64),
    screens: &[Screen],
    popover_w: f64,
    popover_h: f64,
) -> (f64, f64, f64) {
    let (px, py, pw, ph) = tray;
    let contains = |m: &&Screen| {
        let (x, y) = (px / m.scale, py / m.scale);
        x >= m.x / m.scale
            && x < (m.x + m.w) / m.scale
            && y >= m.y / m.scale
            && y < (m.y + m.h) / m.scale
    };
    let on = screens
        .iter()
        .filter(contains)
        .find(|m| MENU_BAR_PT.contains(&(ph / m.scale)))
        .or_else(|| screens.iter().find(contains));
    let scale = on.map_or(1.0, |m| m.scale);
    let (tx, ty, tw, th) = (px / scale, py / scale, pw / scale, ph / scale);
    let mut x = tx + tw / 2.0 - popover_w / 2.0;
    let mut y = ty + th;
    if let Some(m) = on {
        let (left, right) = (m.x / m.scale, (m.x + m.w) / m.scale);
        let (top, bottom) = (m.y / m.scale, (m.y + m.h) / m.scale);
        x = x
            .min(right - popover_w - POPOVER_MARGIN)
            .max(left + POPOVER_MARGIN);
        if ty + th / 2.0 > (top + bottom) / 2.0 {
            // 工作列在下方：開在圖示上方
            y = (ty - popover_h).max(top + POPOVER_MARGIN);
        }
    }
    (x, y, scale)
}

/// 打開主視窗時要它去哪一頁。主視窗關閉只是藏起來，分頁與篩選都還留著，
/// 所以每一次打開都要明確指定目的地，不能留給上一次的狀態決定。
/// 小視窗是不是被使用者拖過大小：程式自己 `set_size` 也會觸發縮放事件，所以和「最後一次要求的大小」比，
/// 差超過 2 個邏輯像素才算使用者拖的（前端分不出來，只在 Rust 端判斷）。
pub fn user_resized(requested: (f64, f64), actual: (f64, f64)) -> bool {
    (requested.0 - actual.0).abs() > 2.0 || (requested.1 - actual.1).abs() > 2.0
}

pub fn main_nav_event(settings: bool) -> &'static str {
    if settings {
        "goto-settings"
    } else {
        "goto-runners"
    }
}

#[cfg(desktop)]
mod desktop {
    use super::*;
    use pr_runners_core::persist::{load_settings, save_settings};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use tauri::image::Image;
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Emitter, Manager, Rect, WebviewWindow, WindowEvent};

    const TRAY_ID: &str = "main";
    const POPOVER_MAX_HEIGHT: f64 = 640.0;
    /// 與 tauri.conf.json 的小視窗 minHeight 一致：程式要求的高度不能低於原生最小值，
    /// 否則原生視窗把它夾回 360，縮放事件和「要求的大小」差很多，會被誤認成使用者拖的。
    const POPOVER_MIN_HEIGHT: f64 = 360.0;
    /// 小視窗最小寬度（與 tauri.conf.json 一致）。
    const POPOVER_MIN_WIDTH: f64 = 340.0;
    /// 點選單列圖示收起小視窗時，失焦先把它藏起來，緊接著的點擊不能又把它打開。
    const REOPEN_GUARD_MS: u64 = 250;
    static LAST_HIDDEN_AT: AtomicU64 = AtomicU64::new(0);
    /// 小視窗目前的高度（邏輯像素，f64 的位元）：開在工作列上方時要知道往上推多少。預設同設定檔的 560。
    static POPOVER_HEIGHT: AtomicU64 = AtomicU64::new(0x4081_8000_0000_0000);
    /// 小視窗目前的寬度（邏輯像素，f64 的位元）：預設 400；使用者拖過就是他拖出來的寬度。
    static POPOVER_WIDTH: AtomicU64 = AtomicU64::new(0x4079_0000_0000_0000);
    /// 程式最後一次要求的大小（寬、高）。縮放事件的大小和它差很多，才是使用者拖的。
    static REQUESTED_SIZE: std::sync::Mutex<(f64, f64)> = std::sync::Mutex::new((400.0, 560.0));
    /// 使用者拖過大小之後，不再隨內容自動調整高度；內容超出就捲動。
    static USER_SIZED: AtomicBool = AtomicBool::new(false);
    /// 使用者拖了大小、還沒存檔（收起小視窗時才存，免得拖曳過程中每個事件都寫一次檔）。
    static SIZE_DIRTY: AtomicBool = AtomicBool::new(false);
    /// 已排了一次延後存檔（拖曳過程中事件很密，合併成一次）。
    static SAVE_PENDING: AtomicBool = AtomicBool::new(false);
    static DATA_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn icon(name: &str) -> Option<Image<'static>> {
        let bytes: &[u8] = match name {
            "ok" => include_bytes!("../icons/tray/ok@2x.png"),
            "warn" => include_bytes!("../icons/tray/warn@2x.png"),
            "critical" => include_bytes!("../icons/tray/critical@2x.png"),
            _ => include_bytes!("../icons/tray/neutral@2x.png"),
        };
        Image::from_bytes(bytes).ok()
    }

    fn hide_popover(w: &WebviewWindow) {
        LAST_HIDDEN_AT.store(now_ms(), Ordering::SeqCst);
        let _ = w.hide();
        save_popover_size();
    }

    /// 把使用者拖出來的大小寫進設定檔（沒拖過或已存過就什麼都不做）。
    fn save_popover_size() {
        if !SIZE_DIRTY.swap(false, Ordering::SeqCst) {
            return;
        }
        let Some(dir) = DATA_DIR.get() else {
            return;
        };
        let (w, h) = *REQUESTED_SIZE.lock().expect("requested size");
        let mut s = load_settings(dir);
        s.popover_size = Some((w.round() as u32, h.round() as u32));
        if let Err(e) = save_settings(dir, &s) {
            eprintln!("小視窗大小存檔失敗：{e}");
        }
    }

    /// 縮放事件：和程式最後一次要求的大小差很多，才是使用者拖的，記下新大小。
    fn on_popover_resized(w: &WebviewWindow, size: &tauri::PhysicalSize<u32>) {
        let scale = w.scale_factor().unwrap_or(1.0);
        let actual = (
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        if actual.0 < 1.0 || actual.1 < 1.0 {
            return; // 收起或最小化時的 0 大小
        }
        let mut requested = REQUESTED_SIZE.lock().expect("requested size");
        if !user_resized(*requested, actual) {
            return;
        }
        *requested = actual;
        POPOVER_WIDTH.store(actual.0.to_bits(), Ordering::SeqCst);
        POPOVER_HEIGHT.store(actual.1.to_bits(), Ordering::SeqCst);
        USER_SIZED.store(true, Ordering::SeqCst);
        SIZE_DIRTY.store(true, Ordering::SeqCst);
        drop(requested);
        // 不能只靠「收起小視窗」才存：使用者拖完直接按「結束」或 App 被結束，就不會經過收起。
        // 拖曳停下來半秒後存一次（事件密集時只排一次）。
        if !SAVE_PENDING.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(600));
                SAVE_PENDING.store(false, Ordering::SeqCst);
                save_popover_size();
            });
        }
    }

    /// 把小視窗移到選單列圖示正下方（見 `popover_origin`）。
    /// 最後一次點擊時，系統匣圖示的 (x, y, w, h)（實體像素）。小視窗高度改變時要用同一個錨點重新定位。
    static TRAY_ANCHOR: std::sync::Mutex<Option<(f64, f64, f64, f64)>> =
        std::sync::Mutex::new(None);

    fn place_popover(app: &AppHandle, w: &WebviewWindow, rect: &Rect) -> Result<(), String> {
        let pos = rect.position.to_physical::<f64>(1.0);
        let size = rect.size.to_physical::<f64>(1.0);
        let tray = (pos.x, pos.y, size.width, size.height);
        *TRAY_ANCHOR.lock().expect("tray anchor") = Some(tray);
        place_at(app, w, tray)
    }

    /// 依圖示位置與目前記住的高度放置小視窗（見 `popover_origin`）。
    fn place_at(
        app: &AppHandle,
        w: &WebviewWindow,
        tray: (f64, f64, f64, f64),
    ) -> Result<(), String> {
        let screens: Vec<Screen> = app
            .available_monitors()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|m| Screen {
                x: f64::from(m.position().x),
                y: f64::from(m.position().y),
                w: f64::from(m.size().width),
                h: f64::from(m.size().height),
                scale: m.scale_factor(),
            })
            .collect();
        let height = f64::from_bits(POPOVER_HEIGHT.load(Ordering::SeqCst));
        let width = f64::from_bits(POPOVER_WIDTH.load(Ordering::SeqCst));
        let (x, y, scale) = popover_origin(tray, &screens, width, height);
        // macOS 的視窗座標是各螢幕通用的「點」；Windows 是實體像素（換回圖示所在螢幕的比例）
        #[cfg(target_os = "macos")]
        let r = {
            let _ = scale;
            w.set_position(tauri::LogicalPosition::new(x, y))
        };
        #[cfg(not(target_os = "macos"))]
        let r = w.set_position(tauri::PhysicalPosition::new(
            (x * scale).round() as i32,
            (y * scale).round() as i32,
        ));
        r.map_err(|e| e.to_string())
    }

    fn toggle_popover(app: &AppHandle, rect: &Rect) {
        let Some(w) = app.get_webview_window("popover") else {
            return;
        };
        if w.is_visible().unwrap_or(false) {
            hide_popover(&w);
        } else if now_ms().saturating_sub(LAST_HIDDEN_AT.load(Ordering::SeqCst)) > REOPEN_GUARD_MS {
            // 移到選單列圖示正下方
            if let Err(e) = place_popover(app, &w, rect) {
                eprintln!("小視窗定位失敗：{e}");
            }
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    pub fn setup(app: &tauri::App) -> tauri::Result<()> {
        TrayIconBuilder::with_id(TRAY_ID)
            .icon(icon("neutral").expect("內建圖示"))
            .icon_as_template(false)
            .show_menu_on_left_click(false)
            .tooltip("pr-runners")
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    rect,
                    ..
                } = event
                {
                    toggle_popover(tray.app_handle(), &rect);
                }
            })
            .build(app)?;

        if let Ok(dir) = app.path().app_data_dir() {
            let _ = DATA_DIR.set(dir.clone());
            // 上次拖出來的大小：有就沿用（之後不再自動調整高度）
            if let Some((w, h)) = load_settings(&dir).popover_size
                && let Some(win) = app.get_webview_window("popover")
            {
                let (w, h) = (
                    f64::from(w).clamp(POPOVER_MIN_WIDTH, 4000.0),
                    f64::from(h).clamp(POPOVER_MIN_HEIGHT, 4000.0),
                );
                POPOVER_WIDTH.store(w.to_bits(), Ordering::SeqCst);
                POPOVER_HEIGHT.store(h.to_bits(), Ordering::SeqCst);
                *REQUESTED_SIZE.lock().expect("requested size") = (w, h);
                USER_SIZED.store(true, Ordering::SeqCst);
                let _ = win.set_size(tauri::LogicalSize::new(w, h));
            }
        }
        if let Some(w) = app.get_webview_window("popover") {
            let w2 = w.clone();
            // 點外面（失焦）就收起；使用者拖大小要記下來
            w.on_window_event(move |e| match e {
                WindowEvent::Focused(false) => hide_popover(&w2),
                WindowEvent::Resized(size) => on_popover_resized(&w2, size),
                _ => {}
            });
        }
        // 主視窗：關閉只是藏起來，App 留在選單列
        if let Some(w) = app.get_webview_window("main") {
            let w2 = w.clone();
            w.on_window_event(move |e| {
                if let WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    let _ = w2.hide();
                }
            });
        }
        Ok(())
    }

    /// 每次收到 snapshot：更新文字與圖示。
    pub fn update(app: &AppHandle, summary: &Summary) {
        let Some(tray) = app.tray_by_id(TRAY_ID) else {
            return;
        };
        let title = tray_title(summary);
        let _ = tray.set_title(if title.is_empty() { None } else { Some(title) });
        let _ = tray.set_tooltip(Some(tray_tooltip(summary)));
        if let Some(img) = icon(tray_icon_for(summary.attention, has_runners(summary))) {
            let _ = tray.set_icon(Some(img));
        }
    }

    /// 顯示主視窗並切到指定頁：`settings` 為 true 是設定頁，否則是 Runners（不帶篩選）。
    pub fn show_main(app: &AppHandle, settings: bool) {
        if let Some(p) = app.get_webview_window("popover") {
            hide_popover(&p);
        }
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
            let _ = w.emit(main_nav_event(settings), ());
        }
    }

    /// 小視窗點某台 runner：打開主視窗並直接顯示那台的執行歷史。
    pub fn show_runner(app: &AppHandle, runner_key: &str) {
        show_main(app, false);
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.emit("goto-runner", runner_key);
        }
    }

    /// 主視窗永遠在最上層（開關由設定決定；啟動時與設定變更時呼叫）。
    pub fn apply_always_on_top(app: &AppHandle, on: bool) {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.set_always_on_top(on);
        }
    }

    /// 內容高度改變時調整小視窗。開在圖示上方時（Windows 工作列）高度一變底部就會離開圖示，
    /// 所以先依同一個錨點重新定位、再改大小，讓底部始終貼著圖示、不被推出螢幕。
    pub fn resize_popover(app: &AppHandle, height: f64) {
        if USER_SIZED.load(Ordering::SeqCst) {
            return; // 使用者拖過大小：不再隨內容變高，內容超出就在視窗內捲動
        }
        if let Some(w) = app.get_webview_window("popover") {
            let h = height.clamp(POPOVER_MIN_HEIGHT, POPOVER_MAX_HEIGHT);
            POPOVER_HEIGHT.store(h.to_bits(), Ordering::SeqCst);
            let anchor = *TRAY_ANCHOR.lock().expect("tray anchor");
            if w.is_visible().unwrap_or(false)
                && let Some(tray) = anchor
                && let Err(e) = place_at(app, &w, tray)
            {
                eprintln!("小視窗重新定位失敗：{e}");
            }
            let width = f64::from_bits(POPOVER_WIDTH.load(Ordering::SeqCst));
            *REQUESTED_SIZE.lock().expect("requested size") = (width, h);
            let _ = w.set_size(tauri::LogicalSize::new(width, h));
        }
    }
}

#[cfg(desktop)]
pub use desktop::{apply_always_on_top, resize_popover, setup, show_main, show_runner, update};

#[cfg(test)]
mod tests {
    use super::*;

    /// 實際的配置：MacBook 內建 Retina（2×，主螢幕）＋左邊外接 2560×1440（1×）。
    fn two_screens() -> Vec<Screen> {
        vec![
            Screen {
                x: 0.0,
                y: 0.0,
                w: 3024.0,
                h: 1964.0,
                scale: 2.0,
            },
            Screen {
                x: -2560.0,
                y: 0.0,
                w: 2560.0,
                h: 1440.0,
                scale: 1.0,
            },
        ]
    }

    /// 外接 1× 螢幕在主螢幕（2×，1512 點寬）右邊：實體範圍 1512..4072 與主螢幕的 0..3024 重疊。
    fn external_on_the_right() -> Vec<Screen> {
        vec![
            Screen {
                x: 0.0,
                y: 0.0,
                w: 3024.0,
                h: 1964.0,
                scale: 2.0,
            },
            Screen {
                x: 1512.0,
                y: 0.0,
                w: 2560.0,
                h: 1440.0,
                scale: 1.0,
            },
        ]
    }

    #[test]
    fn overlapping_physical_ranges_pick_the_screen_the_tray_is_really_on() {
        // 外接螢幕上的圖示（點座標 2000）：實體 2000 也落在主螢幕的 0..3024 裡
        let mut screens = external_on_the_right();
        let expected = (2000.0 + 41.5 - 200.0, 24.0, 1.0);
        assert_eq!(
            popover_origin((2000.0, 0.0, 83.0, 24.0), &screens, 400.0, 560.0),
            expected
        );
        screens.reverse();
        assert_eq!(
            popover_origin((2000.0, 0.0, 83.0, 24.0), &screens, 400.0, 560.0),
            expected,
            "結果不受螢幕列舉順序影響"
        );
        // 主螢幕上的圖示（點座標 1200 → 實體 2400，也落在外接螢幕的 1512..4072 裡）
        assert_eq!(
            popover_origin((2400.0, 0.0, 166.0, 48.0), &screens, 400.0, 560.0),
            (1200.0 + 41.5 - 200.0, 24.0, 2.0)
        );
    }

    #[test]
    fn a_wider_popover_stays_centred_on_the_icon_and_inside_the_screen() {
        let screens = external_on_the_right();
        // 寬度 600：水平置中於圖示（圖示中心 2041.5），不超出螢幕右緣（2560+1512 = 4072 實體 → 點 4072）
        let (x, _, _) = popover_origin((2000.0, 0.0, 83.0, 24.0), &screens, 600.0, 560.0);
        assert_eq!(x, 2041.5 - 300.0);
        // 圖示靠右緣：寬視窗被推回螢幕內
        let (x, _, _) = popover_origin((4000.0, 0.0, 70.0, 24.0), &screens, 600.0, 560.0);
        assert_eq!(x, 4072.0 - 600.0 - 8.0);
    }

    #[test]
    fn only_a_clearly_different_size_counts_as_the_user_dragging() {
        assert!(
            !user_resized((400.0, 560.0), (401.0, 559.0)),
            "差 1px 是程式自己改的"
        );
        assert!(user_resized((400.0, 560.0), (440.0, 560.0)));
        assert!(user_resized((400.0, 560.0), (400.0, 520.0)));
    }

    #[test]
    fn notched_macbook_menu_bar_is_still_recognised() {
        // 有瀏海的內建螢幕：選單列約 37 點 → 實體 74
        let (x, y, _) = popover_origin(
            (2400.0, 0.0, 166.0, 74.0),
            &external_on_the_right(),
            400.0,
            560.0,
        );
        assert_eq!((x, y), (1200.0 + 41.5 - 200.0, 37.0));
    }

    #[test]
    fn popover_sits_under_tray_on_the_1x_external_screen() {
        // 圖示在外接螢幕（點座標 -606, 3，寬 83 高 24）
        let (x, y, _) = popover_origin((-606.0, 3.0, 83.0, 24.0), &two_screens(), 400.0, 560.0);
        assert_eq!((x, y), (-606.0 + 41.5 - 200.0, 27.0));
    }

    #[test]
    fn popover_sits_under_tray_on_the_2x_retina_screen() {
        // 同一個圖示若在 Retina 螢幕（點座標 1200, 0）：實體像素是兩倍，換回點座標要除以 2
        let (x, y, _) = popover_origin((2400.0, 0.0, 166.0, 48.0), &two_screens(), 400.0, 560.0);
        assert_eq!((x, y), (1200.0 + 41.5 - 200.0, 24.0));
    }

    #[test]
    fn popover_never_leaves_the_tray_screen() {
        // 圖示貼著外接螢幕的右緣：小視窗往左推回螢幕內
        let (x, _, _) = popover_origin((-30.0, 3.0, 24.0, 24.0), &two_screens(), 400.0, 560.0);
        assert_eq!(x, -400.0 - 8.0);
        // 圖示貼著左緣
        let (x, _, _) = popover_origin((-2555.0, 3.0, 24.0, 24.0), &two_screens(), 400.0, 560.0);
        assert_eq!(x, -2560.0 + 8.0);
    }

    #[test]
    fn windows_taskbar_at_the_bottom_opens_the_popover_above_the_icon() {
        // 1920×1080、100%：系統匣圖示在右下角工作列（實體 1800,1040，24×40）
        let screen = [Screen {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
            scale: 1.0,
        }];
        let (x, y, scale) = popover_origin((1800.0, 1040.0, 24.0, 40.0), &screen, 400.0, 560.0);
        assert_eq!(scale, 1.0);
        assert_eq!(x, 1920.0 - 400.0 - 8.0, "貼右緣時往左推回螢幕內");
        assert_eq!(y, 1040.0 - 560.0, "開在圖示正上方");
        // 小視窗比圖示上方的空間還高：不超出螢幕頂端
        let (_, y, _) = popover_origin((1800.0, 1040.0, 24.0, 40.0), &screen, 400.0, 1_200.0);
        assert_eq!(y, 8.0);
    }

    #[test]
    fn bottom_anchored_popover_keeps_its_bottom_at_the_icon_whatever_its_height() {
        // 高度變了重新定位時，底部要一直貼著圖示（1040），不能隨著長高被推出螢幕
        let screen = [Screen {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
            scale: 1.0,
        }];
        for h in [120.0, 560.0, 640.0] {
            let (_, y, _) = popover_origin((1800.0, 1040.0, 24.0, 40.0), &screen, 400.0, h);
            assert_eq!(y + h, 1040.0, "高度 {h}");
        }
    }

    #[test]
    fn windows_150_percent_scaling_returns_the_scale_to_convert_back_to_pixels() {
        // 2880×1620 實體、150%：邏輯座標與 100% 的情況一致，呼叫端再乘回 1.5
        let screen = [Screen {
            x: 0.0,
            y: 0.0,
            w: 2880.0,
            h: 1620.0,
            scale: 1.5,
        }];
        let (x, y, scale) = popover_origin((2700.0, 1560.0, 36.0, 60.0), &screen, 400.0, 560.0);
        assert_eq!((x, y, scale), (1920.0 - 400.0 - 8.0, 1040.0 - 560.0, 1.5));
    }

    #[test]
    fn tooltip_carries_the_counts_windows_cannot_show_next_to_the_icon() {
        assert_eq!(tray_tooltip(&s(0, 0, 0, 0, Severity::Ok)), "pr-runners");
        assert_eq!(
            tray_tooltip(&s(4, 1, 0, 7, Severity::Warn)),
            "pr-runners · 4 忙碌 · 1 閒置 · 7 排隊"
        );
        assert_eq!(
            tray_tooltip(&s(1, 0, 2, 0, Severity::Ok)),
            "pr-runners · 1 忙碌 · 0 閒置 · 2 離線 · 0 排隊"
        );
    }

    #[test]
    fn opening_main_window_always_names_a_destination() {
        assert_eq!(main_nav_event(true), "goto-settings");
        assert_eq!(main_nav_event(false), "goto-runners");
    }

    /// 測試用：全部算成自家機器（臨時機器的計數 Task 8 才接上選單列）。
    fn s(busy: u32, idle: u32, offline: u32, waiting: u32, attention: Severity) -> Summary {
        Summary {
            own_busy: busy,
            own_online: busy + idle,
            own_offline: offline,
            pool_busy: 0,
            waiting,
            attention,
            stale: false,
        }
    }

    #[test]
    fn title_shows_busy_count_and_waiting() {
        assert_eq!(tray_title(&s(2, 1, 1, 1, Severity::Warn)), "2▶ 1⏳");
        assert_eq!(
            tray_title(&s(0, 3, 0, 0, Severity::Ok)),
            "0▶",
            "有 runner 但都閒置"
        );
        assert_eq!(tray_title(&s(5, 0, 0, 0, Severity::Ok)), "5▶");
        assert_eq!(
            tray_title(&s(0, 0, 0, 0, Severity::Ok)),
            "",
            "沒有 runner：只有中性圖示"
        );
        assert_eq!(
            tray_title(&s(0, 0, 1, 2, Severity::Ok)),
            "0▶ 2⏳",
            "只有離線 runner 也算有 runner"
        );
    }

    #[test]
    fn icon_is_neutral_without_runners() {
        assert_eq!(tray_icon_for(Severity::Ok, true), "ok");
        assert_eq!(tray_icon_for(Severity::Warn, true), "warn");
        for sev in [Severity::Ok, Severity::Warn] {
            assert_eq!(tray_icon_for(sev, false), "neutral");
        }
    }

    #[test]
    fn has_runners_counts_every_state() {
        assert!(!has_runners(&s(0, 0, 0, 4, Severity::Warn)));
        assert!(has_runners(&s(0, 0, 1, 0, Severity::Ok)));
    }

    #[test]
    fn tray_icon_files_exist_at_1x_and_2x() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/tray");
        for n in ["neutral", "ok", "warn", "critical"] {
            for suffix in ["", "@2x"] {
                assert!(
                    dir.join(format!("{n}{suffix}.png")).exists(),
                    "{n}{suffix}.png"
                );
            }
        }
    }
}
