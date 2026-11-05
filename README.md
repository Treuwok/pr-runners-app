# pr-runners

看 GitHub Actions 的 job 跑在哪台 **self-hosted runner** 上：可以綁一個 PR 看它每個 job 的去向，也可以不綁 PR，直接監控整個 org 的 runner 現在在忙什麼。

```text
🖥 Runner 目前工作  org=acme  16:37:13
共 5 台：🟢2 忙碌  ⚪3 閒置  🔴0 離線
  🟢 vps-1        ← [acme-api push:main←#9] api-ci → core (9m40s)
  🟢 linux-box-1  ← [acme-web PR#42] Web CI → E2E / E2E (shard 1/2) (20m36s)
  ⚪ mac-mini-1 閒置
  ⚪ vps-2   閒置
```

## 安裝

需要 `gh`（已登入）、`jq`、`bash`。

```sh
git clone https://github.com/Treuwok/pr-runners-app.git ~/prj/pr-runners
~/prj/pr-runners/install.sh          # symlink 到 ~/bin/pr-runners；PREFIX=… 可改目錄
```

裝的是 symlink，之後 `git pull` 就是更新。`~/bin/pr-runners` 若原本是一般檔案（以前手動複製的版本），會先備份成 `pr-runners.bak-<時間>` 再換掉。

## 用法

```sh
pr-runners <PR 編號> [--repo owner/name] [--watch] [--local]   # PR 檢視
pr-runners --runners  [--repo owner[/name]] [--watch]          # 只看 runner
pr-runners --help
```

| 選項 | 說明 |
|---|---|
| `--repo` | 指定 `owner/name`；`--runners` 只需要 `<org>`。不給就讀當前 git repo。 |
| `--watch` | 定時重刷。PR 檢視跑完就自動停止；`--runners` 一直跑到 Ctrl-C。 |
| `--local` | 只用在 PR 檢視：只看這個 PR，不掃 org 裡的其他 repo（比較快）。 |

| 環境變數 | 說明 |
|---|---|
| `PR_RUNNERS_ORG` | `--runners` 的預設 org，設了就能在任何目錄直接跑 `pr-runners --runners`。 |
| `PR_RUNNERS_INTERVAL` | `--watch` 重刷秒數，預設 60。每一輪都會掃 org 底下每個 repo，間隔太短會吃光 GitHub 每小時 5,000 次的 API 配額。 |

### 權限

- 基本功能只需要 `repo` + `read:org`：從各 repo「正在跑的 job」反推出哪台 runner 在忙。
- 要看到**閒置／離線**的 runner，需要 `admin:org`（呼叫 `orgs/{org}/actions/runners`）：
  `gh auth refresh -h github.com -s admin:org`。沒有這個權限時，畫面會退回成只顯示忙碌中的 runner，並附上授權提示。

### 畫面上的 `⚠?`

已跑時間顯示 `⚠?`，代表 GitHub API 回傳的開始時間落在**未來**，所以算不出已經跑了多久，HH:MM 也可能跟著偏掉。2026-11-01 實測：GitHub 的 `merged_at`、run／job 的 `created_at`／`started_at` 全部比實際時間多了整整 1 小時，但 merge commit 物件裡的 committer date 是正確的。這是 GitHub 那邊的問題，不是本機時鐘：本機時間、GitHub 回應的 `Date` header、NTP 三方都一致。

## 臨時機器（按需開機的 runner）

有些 runner 服務（例如 Blacksmith）是按需開機：每個 job 一台全新的 VM，跑完就下線，名稱帶一段亂數（`pool-8x-linux-q7k2m9x4b1c8d3e5`）。pr-runners **只靠名稱形狀**把它們和自家 runner 分開（不認任何廠商名稱，也不讀任何 org 設定）：

- 名稱最後一段（以 `-` 切）長度 ≥ 10、只含小寫英數，而且同時含數字與字母 → 臨時機器；去掉最後一段的剩餘部分是它的**組鍵**（`pool-8x-linux`），畫面上只寫「組名 機型」，不寫亂數名稱。
- 尾巴全是字母（約 0.5% 的機率）時，若同組另一台符合上面的條件，或組鍵等於某個 job 的 label（App 看得到 job label，終端版沒抓，不套這一條），也算臨時機器。
- `--runners`：自家 runner 照常一台一行；臨時機器一組一行（`組名 機型  N 在跑`）；離線不再警告：自家離線收成最後一行「離線：a、b」，臨時機器的離線殘骸完全不印。
- PR 檢視：runner 欄寫 `☁ 組名 機型`（只看該 job 自己的 runner 名稱，所以還在排隊、沒有 runner 名稱的 job 仍標 `☁️ hosted`）。
- App：Runners 畫面分「自家機器」與「臨時機器」兩區（見 App 一節）；等臨時機器的排隊 job 也算排隊，超過 10 分鐘會提醒；臨時機器的 job 不進「平常跑多久」的歷史樣本。
- 已知誤判：自家機器若取名 `runner-a1b2c3d4e5` 這種形狀，會被當成臨時機器。

判斷規則在 `crates/rules/src/fleet.rs`；終端版（`bin/pr-runners` 的 `POOL_AWK`）是同一條規則的 awk 版本，兩邊共用案例表 `tests/fixtures/runner-names.tsv`。

## App（第一版）

除了這支終端機工具，這個 repo 也在長出一個 **Mac 選單列＋Windows 系統匣＋Android＋iOS（iPhone）** 的唯讀 App（Rust 核心＋Tauri 2＋Svelte）：用自己的 GitHub 帳號登入，一眼看到所有 self-hosted runner 在跑哪個 repo／workflow、有沒有東西卡住，以及排隊中的 job 哪些 runner 能接。

```text
crates/rules   純規則（時間可信度、排隊對位、需要注意；無 I/O，可編 wasm32）
crates/core    GitHub 用戶端、收集器、Snapshot、本機保存、Device Flow 與金鑰庫
app/           Tauri 2 專案（前端 Svelte 在 app/src，Rust 外殼在 app/src-tauri）
```

### 建置需求

- Rust（edition 2024；`rustup target add wasm32-unknown-unknown` 供 `make check` 用）、pnpm、Node
- Mac 版：Xcode Command Line Tools
- Windows 版：Visual Studio Build Tools（「使用 C++ 的桌面開發」）、WebView2 執行階段（Windows 10／11 通常已內建）。必須在 Windows 上建置
- iOS 版（只能在 Mac 上建置，只做 iPhone）：完整 Xcode、`xcodegen`、CocoaPods；`rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`。簽章用的 Team ID **不寫進 repo**，用環境變數 `APPLE_DEVELOPMENT_TEAM`（Xcode 裡登入的 Apple 帳號底下的 Team ID）
- Android 版：Android SDK／NDK 28.2.13676358、Android Studio 內建的 JDK 21（不要用系統預設的 JDK 25 跑 Gradle）。`source scripts/android-env.sh` 會設好 `ANDROID_HOME`、`NDK_HOME`、`JAVA_HOME`；另需 `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`

### GitHub App 與 client ID

登入走 GitHub App 的 Device Flow，App 只內嵌 client ID（不需要 client secret）。登記步驟見 [`docs/github-app.md`](docs/github-app.md)。client ID 在建置時注入，不寫死在原始碼：

```sh
export PR_RUNNERS_GITHUB_CLIENT_ID=<pr-runners-dev 的 client ID>
export PR_RUNNERS_GITHUB_APP_SLUG=pr-runners-dev      # 安裝頁網址；沒設就是 pr-runners
```

沒設 `PR_RUNNERS_GITHUB_CLIENT_ID` 時，App 的「用 GitHub 登入」會顯示「此版本未設定 GitHub App」（仍可改用 Personal Access Token）。

### 建置與執行

```sh
# Mac
cd app && pnpm install && pnpm tauri dev        # 開發；選單列出現計數與狀態點
cd app && pnpm tauri build                      # 打包

# Mac 開發時不想每次重新編譯都輸入鑰匙圈密碼：用固定的開發者憑證簽章每次編出的程式，
# 鑰匙圈提示時按一次「永遠允許」即可（憑證用 `security find-identity -v -p codesigning` 查）
export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/scripts/dev-sign-run.sh"
export PR_RUNNERS_DEV_SIGN_IDENTITY=<憑證的 SHA-1>
cd app && pnpm tauri dev

# Windows（PowerShell）：產生 .msi 與 .exe 安裝檔在 repo 根目錄的 target\release\bundle\（msi、nsis）
$env:PR_RUNNERS_GITHUB_CLIENT_ID = "<pr-runners-dev 的 client ID>"
$env:PR_RUNNERS_GITHUB_APP_SLUG = "pr-runners-dev"
cd app; pnpm install; pnpm tauri build          # 開發用 pnpm tauri dev

# Android（debug APK，裝到手機或模擬器）
source scripts/android-env.sh
cd app && pnpm tauri android build --debug --apk --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk

# iOS（模擬器）。gen/apple 的 project.yml 等已 commit；Xcode 專案（.xcodeproj）不進 repo（會被寫入你的 Team ID），
# clone 之後先 pnpm tauri ios init --ci 產生它（可重複執行，不會覆寫 project.yml）
export APPLE_DEVELOPMENT_TEAM=<你的 Team ID>
cd app && pnpm tauri ios init --ci && pnpm tauri ios build --debug --target aarch64-sim --ci
xcrun simctl boot "iPhone 17" && xcrun simctl install booted src-tauri/gen/apple/build/arm64-sim/pr-runners.app
xcrun simctl launch booted com.treuwok.prrunners

# iOS（實機）：接上 iPhone、在 Xcode 開 gen/apple 專案選你的裝置，或 pnpm tauri ios dev
# iOS（TestFlight）：封存並匯出 .ipa，再用 Xcode Organizer 或 Transporter 上傳到 App Store Connect
pnpm tauri ios build --export-method app-store-connect --ci
```

iOS 版與 Android 版的差別與限制：

- 沒有選單列／系統匣與小視窗；介面與 Android 共用一欄版面（底部導覽、下拉重新整理），樣式用 iOS 系統色與系統字型（`app/src/styles/ios.css`）。
- token 存在 iOS 的 Data Protection 金鑰庫（`protected`）。**App 必須用 provisioning profile 簽章**，沒簽章會得到 `-34018`（缺少 entitlement）；token 不同步到 iCloud。
- 進背景會被系統凍結，不能背景輪詢；回前景會立刻刷新（與 Android 相同）。
- Bundle ID 是 `com.treuwok.prrunners`，要在你自己的 Apple Developer 帳號下註冊；TestFlight 需要付費的 Apple Developer Program。
- 圖示目前還是 Tauri 預設的 logo（待換成 pr-runners 自己的圖示）。
- CI 不編 iOS（沒有 macOS runner）；改到 `crates/` 或 `app/src-tauri` 後，本機用 `cargo check -p pr-runners-app --target aarch64-apple-ios-sim` 確認 iOS 還編得過。

Windows 版與 Mac 版的差別：

- 系統匣圖示旁邊不能顯示文字，「幾台忙碌、幾個排隊」改放在滑鼠移到圖示上時的提示文字。
- 小視窗會開在系統匣圖示上方（工作列在下方時）。
- token 存在 Windows 認證管理員。
- CI 每次都會在 Windows 上編譯並跑測試；實機安裝與操作要在 Windows 上另外驗收。

建置時可選的前端旗標（預設都沒開，交付用的 APK 不要設）：

| 環境變數 | 作用 |
|---|---|
| `VITE_PR_RUNNERS_SELFTEST=1` | 在登入頁與設定頁顯示「金鑰庫自我檢測」按鈕（驗證 Android Keystore 接線用；`pnpm dev` 自動開啟） |
| `VITE_PR_RUNNERS_MOCK=1` | 整個 App 改用內建的示意資料並在頂端顯示「示意資料（mock）」橫幅（只用於在模擬器上目視驗收版面） |

前端可以在瀏覽器預覽版面（不需要後端）：`cd app && pnpm dev`，開 `http://127.0.0.1:1420/?mock=1`（加 `&platform=android` 看 Android 版）。

## 開發

```sh
make check      # shellcheck + 離線測試 + Rust（cargo test／clippy／rules 的 wasm32 建置／TS 型別一致）
make check-app  # Tauri App crate、Vitest、svelte-check（只在 Mac 本機跑，CI 沒有 webkit2gtk）
make test       # 只跑 bash 測試；BASH_BIN=/bin/bash make test 用 macOS 內建 bash 3.2 跑
```

測試用 `tests/stub/gh`（假的 gh，依 API path 回傳 `tests/fixtures/*.json`）取代真的 gh，不會打 GitHub API。用 `STUB_START_OFFSET` 控制 job 的開始時間（正數就是「在未來」），用 `STUB_NO_ADMIN=1` 模擬沒有 `admin:org`。

## 來源

原本是另一個私有 repo 裡的 `scripts/pr-runners.sh`（#998 第一次加入、#1020 補上 `--runners` 等功能），後來抽出成這個獨立 repo，歷史一起帶了過來（commit trailer `Imported-From:`）。程式碼註解裡的 `原 repo #…` 指的都是那個 repo 的 PR。
