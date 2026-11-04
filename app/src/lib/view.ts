// 純函式：排序與文字格式。畫面元件只負責排版，所有文案與判斷都在這裡（Vitest 測得到）。
// 型別來自 ts-rs 產生的 ./generated（Rust 端是唯一定義）。
import type { AccountStatus } from "./generated/AccountStatus";
import type { CandidateKind } from "./generated/CandidateKind";
import type { Elapsed } from "./generated/Elapsed";
import type { JobOutcome } from "./generated/JobOutcome";
import type { BackfillReport } from "./generated/BackfillReport";
import type { JobRecord } from "./generated/JobRecord";
import type { RosterLevel } from "./generated/RosterLevel";
import type { RunnerView } from "./generated/RunnerView";
import type { Snapshot } from "./generated/Snapshot";
import type { Summary } from "./generated/Summary";
import type { WaitingView } from "./generated/WaitingView";

export const UNTRUSTED = "⚠?";
export const SKEW_NOTE = "⚠? GitHub API 回傳的開始時間在未來（GitHub 端時間戳偏移），已跑時間無法計算。";
export const BUSY_ONLY_NOTE =
  "看不到閒置／離線的 runner：需要 org 管理員安裝本 App 並同意 Self-hosted runners 讀取權限";
export const NO_RUNNERS = "沒有 self-hosted runner";
export const NO_MATCH = "沒有符合的 runner";
export const HOURLY_TARGET_NOTE =
  "GitHub 每個帳號每小時共 5,000 次，與 gh 等其他工具共用。數字越大更新越即時，留給其他工具的越少。";

/** 已跑／已排時間。`null`（沒有可顯示的時間）回傳空字串。 */
export function formatElapsed(e: Elapsed | null): string {
  if (e === null) return "";
  if (e === "Untrusted") return UNTRUSTED;
  const minutes = Math.floor(e.Known.secs / 60);
  if (minutes < 1) return "<1m";
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

const nowSecs = () => Math.floor(Date.now() / 1000);

/** `owner/name` → `name`。 */
export function repoName(repo: string): string {
  const i = repo.indexOf("/");
  return i < 0 ? repo : repo.slice(i + 1);
}

/** 一群 job 裡最常出現的 repo 名稱（畫面頂端只寫它一次）；沒有 job 時 `null`。票數相同取名稱較前的。 */
export function dominantRepo(jobs: { repo: string }[]): string | null {
  const count = new Map<string, number>();
  for (const j of jobs) count.set(repoName(j.repo), (count.get(repoName(j.repo)) ?? 0) + 1);
  let best: string | null = null;
  for (const [name, n] of count) {
    if (best === null || n > count.get(best)! || (n === count.get(best) && name < best)) best = name;
  }
  return best;
}

export const REPO_HUES = 5;

/**
 * 畫面上每個 repo 的識別色（0..REPO_HUES-1）。依名稱排序後依序配色：
 * 5 個以內保證互不相同，同一組 repo 每次重新整理都是同樣的顏色。
 */
export function repoHues(repos: string[]): Map<string, number> {
  const names = [...new Set(repos)].sort();
  return new Map(names.map((n, i) => [n, i % REPO_HUES]));
}

/** job 的顯示名稱：GitHub 原名；只有 repo 不是頂端那一個時才在前面標出 repo。 */
export function jobText(job: { repo: string; job: string }, dominant: string | null): string {
  return repoName(job.repo) === dominant ? job.job : `${repoName(job.repo)} · ${job.job}`;
}

/**
 * Jobs 頁上「這個 job 跑在哪」：Blacksmith（ci-burst 溢出的雲端 VM）、GitHub-hosted、自家 runner 三者分開顯示。
 * Blacksmith 先判斷：它有 runner 名稱，不能被當成自家 runner。沒有可顯示的就是空字串。
 */
export function jobWhere(job: { blacksmith: boolean; hosted: boolean; runner_name: string | null }): string {
  if (job.blacksmith) return "☁ Blacksmith";
  if (job.hosted) return "GitHub-hosted";
  return job.runner_name ?? "";
}

/** 泳道上的文字：忙碌＝正在跑的 job 名稱，其餘是狀態。`prefix` 是 repo 與頂端不同時才有的 repo 名（畫面上用較淡的字）。 */
export function laneLabel(
  r: RunnerView,
  dominant: string | null,
  now: number = nowSecs(),
): { prefix: string | null; text: string } {
  if (r.state === "Busy") {
    if (!r.job) return { prefix: null, text: "忙碌" };
    return { prefix: repoName(r.job.repo) === dominant ? null : repoName(r.job.repo), text: r.job.job };
  }
  if (r.state === "Idle") return { prefix: null, text: "閒置" };
  const since = r.state.Offline.since;
  if (since === "Unknown") return { prefix: null, text: "離線（時間不明）" };
  return { prefix: null, text: `離線 ${Math.max(0, Math.floor((now - since.Observed) / 60))} 分鐘` };
}

/** 同上，合成一行純文字（測試與無障礙標籤用）。 */
export function laneText(r: RunnerView, dominant: string | null, now: number = nowSecs()): string {
  const { prefix, text } = laneLabel(r, dominant, now);
  return prefix ? `${prefix} · ${text}` : text;
}

const MIN_FILL = 4;

/** 泳道條長（百分比）：已跑時間相對於所有泳道裡最久的那一條。時間不可信或沒有時間＝0。 */
export function fillPercent(e: Elapsed | null, maxSecs: number): number {
  if (e === null || e === "Untrusted" || maxSecs <= 0) return 0;
  return Math.min(100, Math.max(MIN_FILL, Math.round((e.Known.secs / maxSecs) * 100)));
}

export function maxElapsedSecs(rs: RunnerView[]): number {
  return Math.max(0, ...rs.map((r) => (r.elapsed && r.elapsed !== "Untrusted" ? r.elapsed.Known.secs : 0)));
}

/** 預估剩餘時間：有「平常跑多久」才預估；沒有就是 none（畫面只顯示已跑時間）。 */
export type Eta =
  | { kind: "none" }
  | { kind: "ontrack"; pct: number; remainingMin: number; typicalMin: number }
  | { kind: "over"; overMin: number; typicalMin: number };

export function eta(e: Elapsed | null, typicalSecs: number | null | undefined): Eta {
  if (!typicalSecs || typicalSecs <= 0 || e === null || e === "Untrusted") return { kind: "none" };
  const secs = e.Known.secs;
  const typicalMin = Math.max(1, Math.round(typicalSecs / 60));
  if (secs <= typicalSecs) {
    return {
      kind: "ontrack",
      pct: Math.max(MIN_FILL, Math.min(100, Math.round((secs / typicalSecs) * 100))),
      remainingMin: Math.max(1, Math.ceil((typicalSecs - secs) / 60)),
      typicalMin,
    };
  }
  return { kind: "over", overMin: Math.max(1, Math.floor((secs - typicalSecs) / 60)), typicalMin };
}

/** 時間欄的主文字；沒有預估時回傳 null（畫面改顯示已跑時間）。 */
export function etaText(x: Eta): string | null {
  if (x.kind === "ontrack") return `還要約 ${x.remainingMin} 分`;
  if (x.kind === "over") return `比平常久 ${x.overMin} 分`;
  return null;
}

export type QueueJob = { name: string; count: number; url: string | null; noMatch: boolean };
export type QueueGroup = {
  key: string;
  repo: string;
  workflow: string;
  jobs: QueueJob[];
  oldest: Elapsed | null;
  attention: boolean;
};

const knownSecs = (e: Elapsed | null) => (e && e !== "Untrusted" ? e.Known.secs : -1);

/**
 * 排隊中的 job 依 repo＋workflow 歸成一組，組內同名的合併成「×n」（保留 GitHub 原名）。
 * 只有單一個 job 時附上連結；`noMatch`＝名冊完整卻沒有任何 runner 能接。組與組之間最久的排前面。
 */
export function groupQueue(items: { w: WaitingView; roster: RosterLevel }[]): QueueGroup[] {
  const groups = new Map<string, QueueGroup>();
  for (const { w, roster } of items) {
    const key = `${w.job.repo}\u0000${w.job.workflow}`;
    let g = groups.get(key);
    if (!g) {
      g = { key, repo: w.job.repo, workflow: w.job.workflow, jobs: [], oldest: null, attention: false };
      groups.set(key, g);
    }
    const noMatch = roster === "Full" && w.candidates.length === 0;
    const same = g.jobs.find((j) => j.name === w.job.job);
    if (same) {
      same.count += 1;
      same.url = null;
      same.noMatch ||= noMatch;
    } else {
      g.jobs.push({ name: w.job.job, count: 1, url: w.job.url, noMatch });
    }
    if (g.oldest === null || knownSecs(w.age) > knownSecs(g.oldest)) g.oldest = w.age;
    g.attention ||= w.attention;
  }
  return [...groups.values()].sort((a, b) => knownSecs(b.oldest) - knownSecs(a.oldest));
}

/** 授權後第一次列出範圍時還沒有任何勾選：預設全選（只在「一個都沒選」時；已有選擇就原樣保留）。 */
export function defaultSelected<T extends { selected: boolean }>(choices: T[]): T[] {
  return choices.some((c) => c.selected) ? choices : choices.map((c) => ({ ...c, selected: true }));
}

function rank(r: RunnerView): number {
  if (r.attention) return 0;
  if (r.state === "Busy") return 1;
  if (r.state === "Idle") return 3;
  return 2; // Offline（沒有被判定需要注意的）
}

/** 需要注意優先 → 忙碌 → 離線 → 閒置 → 名稱。回傳新陣列，不改動輸入。 */
export function sortRunners(rs: RunnerView[]): RunnerView[] {
  return [...rs].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
}

/** 任何地方出現「時間不可信」就在底部加註。 */
export function hasSkew(snap: Snapshot): boolean {
  return snap.accounts.some((a) =>
    a.scopes.some(
      (s) =>
        s.runners.some((r) => r.elapsed === "Untrusted") ||
        s.waiting.some((w) => w.age === "Untrusted"),
    ),
  );
}

/** 頂端四格的計數：從「目前畫面上看得到的」runner 與排隊算，選了單一範圍時數字才會跟清單一致。 */
export function countVisible(runners: RunnerView[], waiting: number): Summary {
  let busy = 0;
  let idle = 0;
  let offline = 0;
  for (const r of runners) {
    if (r.state === "Busy") busy += 1;
    else if (r.state === "Idle") idle += 1;
    else offline += 1;
  }
  const attention: Summary["attention"] = runners.some((r) => r.attention) ? "Critical" : "Ok";
  return { busy, idle, offline, waiting, attention };
}

/** 標題列副標：忙碌、閒置、排隊；離線只在有的時候才出現。 */
export function summaryLine(s: Summary): string {
  const parts = [`${s.busy} 忙碌`, `${s.idle} 閒置`];
  if (s.offline > 0) parts.push(`${s.offline} 離線`);
  parts.push(`${s.waiting} 排隊`);
  return parts.join(" · ");
}

export type Dot = "busy" | "idle" | "offline" | "attention";

/** 狀態點顏色：需要注意＝橘或紅由 attention 決定（離線且需要注意＝紅）。 */
export function dotFor(r: RunnerView): Dot {
  if (typeof r.state === "object") return "offline";
  return r.state === "Busy" ? "busy" : "idle";
}

/** HH:MM（本機時區）。 */
export function formatClock(unixSecs: number): string {
  const d = new Date(unixSecs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** 帳號狀態在畫面底部（StatusFooter）的文字；`null`＝正常，只顯示「最後更新」。 */
export function accountNote(status: AccountStatus): string | null {
  if (status === "Ok") return null;
  if (status === "NeedsReauth") return "重新登入";
  if ("Throttled" in status) {
    return `已放慢更新（剩 ${status.Throttled.remaining} 次／重置於 ${formatClock(status.Throttled.reset_at)}）`;
  }
  return `最後更新 ${formatClock(status.Offline.last_ok_at)}`;
}

/** 選單列／app bar 計數用的文字：沒有任何 runner 時為空字串。 */
export function trayTitleLike(s: Summary): string {
  if (s.busy + s.idle + s.offline === 0) return "";
  return s.waiting > 0 ? `${s.busy}▶ ${s.waiting}⏳` : `${s.busy}▶`;
}

// ───────────── runner 執行歷史 ─────────────

export type OutcomeTone = "ok" | "bad" | "run" | "muted";

/** 結果的中文標籤與色調（色調只給狀態點與標籤用，文字一律搭配標籤）。 */
export function outcomeLabel(o: JobOutcome): { text: string; tone: OutcomeTone } {
  switch (o) {
    case "Running":
      return { text: "執行中", tone: "run" };
    case "Success":
      return { text: "成功", tone: "ok" };
    case "Failure":
      return { text: "失敗", tone: "bad" };
    case "TimedOut":
      return { text: "逾時", tone: "bad" };
    case "Cancelled":
      return { text: "取消", tone: "muted" };
    case "Skipped":
      return { text: "略過", tone: "muted" };
    case "Other":
      return { text: "其他", tone: "muted" };
  }
}

/** 跑了多久：已結束用結束時間，還在跑用現在；沒有開始時間就空字串。 */
export function recordDuration(r: JobRecord, now: number = nowSecs()): string {
  if (r.started_at === null) return "";
  const end = r.completed_at ?? (r.outcome === "Running" ? now : null);
  if (end === null) return "";
  return formatElapsed({ Known: { secs: Math.max(0, end - r.started_at) } });
}

/** 「今天 21:03」「昨天 08:15」「11/01 21:03」（本機時區）。 */
export function recordWhen(ts: number | null, now: number = nowSecs()): string {
  if (ts === null) return "時間不明";
  const d = new Date(ts * 1000);
  const today = new Date(now * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  const hm = `${p(d.getHours())}:${p(d.getMinutes())}`;
  const dayStart = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((dayStart(today) - dayStart(d)) / 86_400_000);
  if (days === 0) return `今天 ${hm}`;
  if (days === 1) return `昨天 ${hm}`;
  return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${hm}`;
}

export type HistoryFilter = "all" | "ok" | "bad" | "other";

/** 篩選：成功／失敗（含逾時）／其他（取消、略過、執行中…）。 */
export function filterRecords(rs: JobRecord[], f: HistoryFilter): JobRecord[] {
  if (f === "all") return rs;
  return rs.filter((r) => {
    const tone = outcomeLabel(r.outcome).tone;
    return f === "ok" ? tone === "ok" : f === "bad" ? tone === "bad" : tone === "muted" || tone === "run";
  });
}

export function countRecords(rs: JobRecord[]): Record<HistoryFilter, number> {
  return {
    all: rs.length,
    ok: filterRecords(rs, "ok").length,
    bad: filterRecords(rs, "bad").length,
    other: filterRecords(rs, "other").length,
  };
}

/** 依 runner key 在 snapshot 裡找到那台 runner 與它所屬的帳號、範圍（補查要用）。 */
export function findRunner(
  snap: Snapshot,
  key: string,
): { runner: RunnerView; accountId: number; scope: string } | null {
  for (const a of snap.accounts) {
    for (const s of a.scopes) {
      const runner = s.runners.find((r) => r.key === key);
      if (runner) return { runner, accountId: a.account_id, scope: s.name };
    }
  }
  return null;
}

/** 補查完的說明。 */
export function backfillNote(r: BackfillReport): string {
  if (r.repos_checked === 0 && !r.partial) return "最近 7 天沒有推送過的 repo，沒有可以補查的紀錄";
  const base = `查了 ${r.repos_checked} 個 repo、${r.runs_checked} 次執行，補到 ${r.added} 筆`;
  return r.partial ? `${base}（有些 repo 查不到，可能是權限或網路）` : base;
}
