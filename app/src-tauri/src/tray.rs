//! 選單列（macOS）與系統匣（Windows）：圖示由 `tray_icon` 依 `Summary` 即時畫出，左鍵點開小視窗（popover）。
//! 文字（tooltip）與圖示只由 Rust 端算（前端不重寫一份）。

use pr_runners_core::view::Summary;

/// 有沒有 runner（決定是否畫中性的空心格）：只看自家機器與正在跑的臨時機器，離線的臨時機器殘骸不算。
pub fn has_runners(s: &Summary) -> bool {
    s.own_online + s.own_offline + s.pool_busy > 0
}

/// 圖示的提示文字：`自家 2/5 在忙 · 臨時 9 台在跑 · 排隊 1`。沒有臨時機器在跑時省略中段；
/// 離線只算自家機器，0 台時省略。Windows 的系統匣放不下數字，計數只看得到這裡。
pub fn tray_tooltip(s: &Summary) -> String {
    if !has_runners(s) {
        return "pr-runners".into();
    }
    let mut parts = vec![format!("自家 {}/{} 在忙", s.own_busy, s.own_online)];
    if s.pool_busy > 0 {
        parts.push(format!("臨時 {} 台在跑", s.pool_busy));
    }
    parts.push(format!("排隊 {}", s.waiting));
    if s.own_offline > 0 {
        parts.push(format!("離線 {}", s.own_offline));
    }
    parts.join(" · ")
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
    use crate::tray_icon::Ink;
    #[cfg(target_os = "macos")]
    use crate::tray_icon::render_menubar;
    #[cfg(not(target_os = "macos"))]
    use crate::tray_icon::render_square;
    use pr_runners_core::persist::{load_settings, save_settings};
    use pr_runners_core::view::Severity;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use tauri::image::Image;
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Emitter, Manager, Rect, WebviewWindow, WindowEvent};

    const TRAY_ID: &str = "main";
    const POPOVER_MAX_HEIGHT: f64 = 640.0;
    const POPOVER_MIN_HEIGHT: f64 = 120.0;
    /// 小視窗最小尺寸（與 tauri.conf.json 一致）。
    const POPOVER_MIN_WIDTH: f64 = 340.0;
    const POPOVER_MIN_SIZE_HEIGHT: f64 = 360.0;
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
    static DATA_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// 沒有任何資料時的初始圖示：中性的空心格。
    fn empty_summary() -> Summary {
        Summary {
            own_busy: 0,
            own_online: 0,
            own_offline: 0,
            pool_busy: 0,
            waiting: 0,
            attention: Severity::Ok,
            stale: false,
        }
    }

    /// 最近一次收到的 `Summary` 與畫出去時用的墨水：選單列深淺改變時要用同一份資料重畫。
    static LAST_SUMMARY: std::sync::Mutex<Option<Summary>> = std::sync::Mutex::new(None);
    static LAST_INK: std::sync::Mutex<Option<Ink>> = std::sync::Mutex::new(None);

    /// macOS：讀選單列圖示按鈕自己的外觀（不是 App 主題：新版 macOS 的選單列會依桌布變深變淺）。
    /// 回傳外觀名稱（例如 `NSAppearanceNameDarkAqua`）；讀不到就是 `None`。
    #[cfg(target_os = "macos")]
    fn menubar_appearance_name(app: &AppHandle) -> Option<String> {
        use objc2::{msg_send, rc::Retained, runtime::AnyObject};
        let tray = app.tray_by_id(TRAY_ID)?;
        tray.with_inner_tray_icon(|inner| {
            let item = inner.ns_status_item()?;
            // SAFETY：with_inner_tray_icon 在主執行緒執行；這些都是 AppKit 的唯讀屬性，回傳值可能是 nil（Option）。
            unsafe {
                let button: Option<Retained<AnyObject>> = msg_send![&*item, button];
                let appearance: Option<Retained<AnyObject>> =
                    msg_send![&*button?, effectiveAppearance];
                let name: Option<Retained<AnyObject>> = msg_send![&*appearance?, name];
                let c: *const std::ffi::c_char = msg_send![&*name?, UTF8String];
                if c.is_null() {
                    return None;
                }
                std::ffi::CStr::from_ptr(c).to_str().ok().map(str::to_owned)
            }
        })
        .ok()
        .flatten()
    }

    /// 這次要用哪種墨水。平常一律樣板圖（系統自動配深淺色）；只有排隊太久、而且讀得到選單列外觀時，
    /// 才依外觀畫黑或白，好讓小牌用橘色。
    fn ink_for(app: &AppHandle, s: &Summary) -> Ink {
        #[cfg(target_os = "macos")]
        if s.attention == Severity::Warn {
            let name = menubar_appearance_name(app);
            if std::env::var_os("PR_RUNNERS_TRAY_DEBUG").is_some() {
                eprintln!("[tray] 選單列外觀：{name:?}");
            }
            if let Some(name) = name {
                return if name.contains("Dark") {
                    Ink::Dark
                } else {
                    Ink::Light
                };
            }
        }
        let _ = (app, s);
        Ink::Template
    }

    fn art_image(s: &Summary, ink: Ink) -> (Image<'static>, bool) {
        #[cfg(target_os = "macos")]
        let art = render_menubar(s, ink, 2.0);
        #[cfg(not(target_os = "macos"))]
        let art = {
            let _ = ink;
            render_square(s, 2.0)
        };
        let template = art.template;
        (Image::new_owned(art.rgba, art.width, art.height), template)
    }

    fn apply(app: &AppHandle, s: &Summary, ink: Ink) {
        let Some(tray) = app.tray_by_id(TRAY_ID) else {
            return;
        };
        let (img, template) = art_image(s, ink);
        let _ = tray.set_icon(Some(img));
        let _ = tray.set_icon_as_template(template);
        // 數字都畫進圖裡了，不再用 set_title 放文字
        let _ = tray.set_title(None::<String>);
        let _ = tray.set_tooltip(Some(tray_tooltip(s)));
        *LAST_INK.lock().expect("last ink") = Some(ink);
    }

    /// 選單列深淺改變（切換深淺色、換桌布）時重畫。只有橘色狀態（依外觀畫黑或白）才會受影響，
    /// 所以平常什麼都不做；每 2 秒看一次，沒變就不重畫。
    #[cfg(target_os = "macos")]
    fn watch_appearance(app: AppHandle) {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let Some(s) = LAST_SUMMARY.lock().expect("last summary").clone() else {
                    continue;
                };
                if s.attention != Severity::Warn {
                    continue;
                }
                let ink = ink_for(&app, &s);
                if *LAST_INK.lock().expect("last ink") != Some(ink) {
                    apply(&app, &s, ink);
                }
            }
        });
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
            .icon(art_image(&empty_summary(), Ink::Template).0)
            .icon_as_template(cfg!(target_os = "macos"))
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

        #[cfg(target_os = "macos")]
        watch_appearance(app.handle().clone());
        if let Ok(dir) = app.path().app_data_dir() {
            let _ = DATA_DIR.set(dir.clone());
            // 上次拖出來的大小：有就沿用（之後不再自動調整高度）
            if let Some((w, h)) = load_settings(&dir).popover_size
                && let Some(win) = app.get_webview_window("popover")
            {
                let (w, h) = (
                    f64::from(w).clamp(POPOVER_MIN_WIDTH, 4000.0),
                    f64::from(h).clamp(POPOVER_MIN_SIZE_HEIGHT, 4000.0),
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

    /// 每次收到 snapshot：重畫圖示、更新 tooltip。
    pub fn update(app: &AppHandle, summary: &Summary) {
        *LAST_SUMMARY.lock().expect("last summary") = Some(summary.clone());
        apply(app, summary, ink_for(app, summary));
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

    fn sum(
        own_busy: u32,
        own_online: u32,
        own_offline: u32,
        pool_busy: u32,
        waiting: u32,
    ) -> Summary {
        Summary {
            own_busy,
            own_online,
            own_offline,
            pool_busy,
            waiting,
            attention: pr_runners_core::view::Severity::Ok,
            stale: false,
        }
    }

    #[test]
    fn tooltip_carries_the_counts_windows_cannot_show_next_to_the_icon() {
        assert_eq!(tray_tooltip(&sum(0, 0, 0, 0, 0)), "pr-runners");
        assert_eq!(
            tray_tooltip(&sum(2, 5, 0, 9, 1)),
            "自家 2/5 在忙 · 臨時 9 台在跑 · 排隊 1",
            "有臨時機器"
        );
        assert_eq!(
            tray_tooltip(&sum(4, 5, 0, 0, 7)),
            "自家 4/5 在忙 · 排隊 7",
            "沒有臨時機器：省略那一段"
        );
        assert_eq!(
            tray_tooltip(&sum(1, 1, 2, 0, 0)),
            "自家 1/1 在忙 · 排隊 0 · 離線 2",
            "有自家離線"
        );
    }

    #[test]
    fn has_runners_ignores_offline_pool_leftovers() {
        // 臨時機器全部結束（開關關了）：名冊上只剩離線殘骸，Summary 裡沒有任何計數
        assert!(!has_runners(&sum(0, 0, 0, 0, 4)));
        assert!(
            has_runners(&sum(0, 0, 1, 0, 0)),
            "只有離線的自家機器也算有 runner"
        );
        assert!(has_runners(&sum(0, 0, 0, 3, 0)), "只有臨時機器在跑也算");
    }

    #[test]
    fn opening_main_window_always_names_a_destination() {
        assert_eq!(main_nav_event(true), "goto-settings");
        assert_eq!(main_nav_event(false), "goto-runners");
    }
}
