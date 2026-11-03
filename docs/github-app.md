# 登記 GitHub App（給作者照做的清單）

App 第一版用 **GitHub App ＋ Device Flow** 登入，所以 App 裡只內嵌 client ID，不需要 client secret，也沒有任何中間伺服器。
需要在 **Treuwok org** 各登記兩個 App：正式的 `pr-runners`，和開發測試用的 `pr-runners-dev`。
兩個的設定完全相同，只有名稱不同。

## 登記步驟

在 GitHub 的 *Settings → Developer settings → GitHub Apps → New GitHub App*（要在 Treuwok org 底下建，不是個人帳號）。

| 欄位 | 設定 | 為什麼 |
|---|---|---|
| GitHub App name | `pr-runners`（另一個是 `pr-runners-dev`） | 安裝頁網址是 `github.com/apps/<name>/installations/new` |
| Homepage URL | 隨意（例如 repo 網址） | 必填欄位，不影響功能 |
| Callback URL | 不需要 | Device Flow 不用 callback |
| Expire user authorization tokens | **開** | token 8 小時過期、refresh token 6 個月；App 會自動續期 |
| Request user authorization (OAuth) during installation | 不勾 | 不需要 |
| **Enable Device Flow** | **勾** | 登入流程就是 Device Flow |
| Webhook → Active | **關** | 第一版沒有後端，不收 webhook |
| Where can this GitHub App be installed? | **Any account（Public）** | Private 的 App 只能裝在 Treuwok 自己，裝不到個人帳號。**兩個 App 都要設成 Public** |

### 權限（全部唯讀）

- **Repository permissions**
  - Actions：Read-only
  - Administration：Read-only（讀 repo 層級的 runner）
  - Metadata：Read-only（必選）
  - Pull requests：Read-only（用 `commits/{sha}/pulls` 反查 PR 標籤）
- **Organization permissions**
  - Self-hosted runners：Read-only

不要多給任何寫入權限。第一版是唯讀 App。

## 記下 client ID

建好之後，App 頁面最上面有 **Client ID**（長得像 `Iv23li…`）。這不是機密，但也不寫死在原始碼裡：建置時從環境變數注入。

```bash
export PR_RUNNERS_GITHUB_CLIENT_ID=<pr-runners-dev 的 client ID>   # 開發與測試期間用 dev 版
export PR_RUNNERS_GITHUB_APP_SLUG=pr-runners-dev                   # 安裝頁網址用；沒設就是 pr-runners
```

沒設 `PR_RUNNERS_GITHUB_CLIENT_ID` 的話，App 的「用 GitHub 登入」會顯示「此版本未設定 GitHub App」。

## 安裝到帳號

登記完之後，到 `https://github.com/apps/pr-runners-dev/installations/new`，分別安裝到：

1. **Treuwok**（org）：選 All repositories，或只選要監控的 repo
2. **作者的個人帳號**

org 的 runner 名冊需要 org 管理員同意 *Self-hosted runners: Read*。沒有同意時，App 會降級成只顯示正在跑的 runner，並提示怎麼補。

## 備用：Personal Access Token

不能安裝 GitHub App 的 org，可以改貼 fine-grained PAT（同樣只給唯讀權限：Actions、Administration、Metadata、Pull requests 的 Read，加上 org 的 Self-hosted runners Read）。PAT 的可勾選範圍來自 `GET /user/repos` 回傳的 repo 擁有者，不靠 `/user/orgs`（fine-grained token 在那個端點會得到 200 與空清單）。
