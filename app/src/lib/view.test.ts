import { describe, expect, it } from "vitest";
import dev from "./dev-snapshot.json";
import type { AccountView } from "./generated/AccountView";
import type { JobView } from "./generated/JobView";
import type { RunnerView } from "./generated/RunnerView";
import type { Snapshot } from "./generated/Snapshot";
import type { JobRecord } from "./generated/JobRecord";
import type { WaitingView } from "./generated/WaitingView";
import type { Summary } from "./generated/Summary";
import {
  detectPlatform,
  isDesktop,
  isMobile,
  splitRunners,
  waitLabel,
  accountNote,
  dotFor,
  formatClock,
  formatElapsed,
  hasSkew,
  repoName,
  countVisible,
  defaultSelected,
  dominantRepo,
  eta,
  etaText,
  backfillNote,
  countRecords,
  filterRecords,
  findRunner,
  outcomeLabel,
  recordDuration,
  recordWhen,
  fillPercent,
  groupQueue,
  jobText,
  jobWhere,
  laneText,
  maxElapsedSecs,
  REPO_HUES,
  repoHues,
  sortRunners,
  summaryLine,
  trayTitleLike,
} from "./view";

const NOW = 1_800_000_000;

function runner(over: Partial<RunnerView> = {}): RunnerView {
  return {
    key: "1/o/1",
    name: "r",
    os: "Linux",
    state: "Idle",
    labels: [],
    group: null,
    job: null,
    elapsed: null,
    pool: null,
    can_take: null,
    ...over,
  };
}
const job = {
  key: "1/2",
  repo: "Acme/acme-app",
  workflow: "app-ci",
  job: "device-e2e",
  state: "InProgress" as const,
  labels: [],
  pr_tag: "PR#1",
  url: "https://github.com/x",
  runner_name: "r",
  hosted: false,
  pool: null,
  typical_secs: null as number | null,
};

describe("formatElapsed", () => {
  it("Known：分鐘與小時", () => {
    expect(formatElapsed({ Known: { secs: 540 } })).toBe("9m");
    expect(formatElapsed({ Known: { secs: 60 } })).toBe("1m");
    expect(formatElapsed({ Known: { secs: 59 * 60 + 59 } })).toBe("59m");
    expect(formatElapsed({ Known: { secs: 3600 } })).toBe("1h 0m");
    expect(formatElapsed({ Known: { secs: 3600 + 4 * 60 } })).toBe("1h 4m");
    expect(formatElapsed({ Known: { secs: 26 * 3600 + 5 * 60 } })).toBe("26h 5m");
  });
  it("不到 1 分鐘", () => {
    expect(formatElapsed({ Known: { secs: 0 } })).toBe("<1m");
    expect(formatElapsed({ Known: { secs: 59 } })).toBe("<1m");
  });
  it("Untrusted 顯示 ⚠?，null 為空", () => {
    expect(formatElapsed("Untrusted")).toBe("⚠?");
    expect(formatElapsed(null)).toBe("");
  });
});

describe("laneText", () => {
  it("忙碌：GitHub 上的 job 原名；repo 就是頂端那個時不重複", () => {
    expect(laneText(runner({ state: "Busy", job }), "acme-app", NOW)).toBe("device-e2e");
  });
  it("忙碌：repo 與頂端不同時才在前面標出", () => {
    expect(laneText(runner({ state: "Busy", job }), "acme-web", NOW)).toBe("acme-app · device-e2e");
    expect(laneText(runner({ state: "Busy", job }), null, NOW)).toBe("acme-app · device-e2e");
  });
  it("job 名稱含括號與斜線時原樣保留", () => {
    const j = { ...job, job: "E2E (shard 2/2)" };
    expect(laneText(runner({ state: "Busy", job: j }), "acme-app", NOW)).toBe("E2E (shard 2/2)");
  });
  it("忙碌但看不到 job", () => {
    expect(laneText(runner({ state: "Busy" }), null, NOW)).toBe("忙碌");
  });
  it("閒置", () => {
    expect(laneText(runner({ state: "Idle" }), null, NOW)).toBe("閒置");
  });
  it("離線：觀察到的起點算分鐘，時間不明另有文案", () => {
    const off = (since: RunnerView["state"]) => runner({ state: since });
    expect(laneText(off({ Offline: { since: { Observed: NOW - 4 * 60 - 30 } } }), null, NOW)).toBe("離線 4 分鐘");
    expect(laneText(off({ Offline: { since: { Observed: NOW } } }), null, NOW)).toBe("離線 0 分鐘");
    expect(laneText(off({ Offline: { since: "Unknown" } }), null, NOW)).toBe("離線（時間不明）");
  });
  it("起點在未來（本機時鐘落後）不顯示負數", () => {
    const r = runner({ state: { Offline: { since: { Observed: NOW + 600 } } } });
    expect(laneText(r, null, NOW)).toBe("離線 0 分鐘");
  });
});

describe("dominantRepo / jobText", () => {
  it("最常出現的 repo；票數相同取名稱較前的；沒有 job 為 null", () => {
    const j = (repo: string) => ({ repo });
    expect(dominantRepo([j("o/b"), j("o/a"), j("o/b")])).toBe("b");
    expect(dominantRepo([j("o/b"), j("o/a")])).toBe("a");
    expect(dominantRepo([])).toBeNull();
  });
  it("只有 repo 不同時才加前綴", () => {
    expect(jobText({ repo: "o/a", job: "Unit" }, "a")).toBe("Unit");
    expect(jobText({ repo: "o/b", job: "Unit" }, "a")).toBe("b · Unit");
  });
});

describe("repoHues", () => {
  it("五個以內互不同色，重複名稱只算一次，順序不影響結果", () => {
    const m = repoHues(["acme-harness", "acme-web", "acme-app", "acme-web"]);
    expect(m.size).toBe(3);
    expect(new Set(m.values()).size).toBe(3);
    expect(repoHues(["acme-app", "acme-harness", "acme-web"])).toEqual(m);
  });
  it("超過五個才開始重複，且都在色盤範圍內", () => {
    const m = repoHues(["a", "b", "c", "d", "e", "f"]);
    expect([...m.values()]).toEqual([0, 1, 2, 3, 4, 0]);
    expect(Math.max(...m.values())).toBeLessThan(REPO_HUES);
  });
});

describe("fillPercent / maxElapsedSecs", () => {
  const k = (secs: number) => ({ Known: { secs } });
  it("相對於最久的那一條；太短的留一小段；滿格不超過 100", () => {
    expect(fillPercent(k(1860), 1860)).toBe(100);
    expect(fillPercent(k(930), 1860)).toBe(50);
    expect(fillPercent(k(10), 1860)).toBe(4);
    expect(fillPercent(k(5000), 1860)).toBe(100);
  });
  it("沒有時間、時間不可信、或沒有最大值＝0（不畫一個假的長度）", () => {
    expect(fillPercent(null, 1860)).toBe(0);
    expect(fillPercent("Untrusted", 1860)).toBe(0);
    expect(fillPercent(k(60), 0)).toBe(0);
  });
  it("最大值忽略不可信的時間", () => {
    expect(maxElapsedSecs([runner({ elapsed: k(60) }), runner({ elapsed: "Untrusted" }), runner({ elapsed: k(600) })])).toBe(600);
    expect(maxElapsedSecs([])).toBe(0);
  });
});

describe("eta / etaText", () => {
  const k = (secs: number) => ({ Known: { secs } });
  it("還在平常時間內：進度＝已跑 ÷ 平常，剩餘無條件進位到分鐘", () => {
    const x = eta(k(20 * 60), 28 * 60);
    expect(x).toEqual({ kind: "ontrack", pct: 71, remainingMin: 8, typicalMin: 28 });
    expect(etaText(x)).toBe("還要約 8 分");
    expect(eta(k(28 * 60 - 10), 28 * 60)).toMatchObject({ kind: "ontrack", remainingMin: 1 });
  });
  it("超過平常時間：顯示多久了，至少 1 分", () => {
    expect(eta(k(31 * 60), 28 * 60)).toEqual({ kind: "over", overMin: 3, typicalMin: 28 });
    expect(etaText(eta(k(28 * 60 + 5), 28 * 60))).toBe("比平常久 1 分");
  });
  it("沒有紀錄、時間不可信或沒有已跑時間：不預估", () => {
    expect(eta(k(60), null)).toEqual({ kind: "none" });
    expect(eta(k(60), undefined)).toEqual({ kind: "none" });
    expect(eta("Untrusted", 600)).toEqual({ kind: "none" });
    expect(eta(null, 600)).toEqual({ kind: "none" });
    expect(etaText({ kind: "none" })).toBeNull();
  });
});

describe("groupQueue", () => {
  const w = (name: string, workflow: string, secs: number | "Untrusted", over: Partial<WaitingView> = {}): WaitingView => ({
    job: { ...job, state: "Queued", job: name, workflow, key: `${workflow}/${name}/${secs}`, url: `https://github.com/${name}` },
    age: secs === "Untrusted" ? "Untrusted" : { Known: { secs } },
    attention: false,
    candidates: [{ runner_key: "1/o/1", runner_name: "r", kind: "CanTakeBusy" }],
    ...over,
  });
  const full = (x: WaitingView) => ({ w: x, roster: "Full" as const });
  it("同名 job 合併成 ×n，原名不改；單一個才有連結", () => {
    const [g] = groupQueue([full(w("Unit Tests", "ci", 600)), full(w("Integration Tests", "ci", 700)), full(w("Integration Tests", "ci", 650))]);
    expect(g.jobs.map((j) => [j.name, j.count, j.url])).toEqual([
      ["Unit Tests", 1, "https://github.com/Unit Tests"],
      ["Integration Tests", 2, null],
    ]);
    expect(g.oldest).toEqual({ Known: { secs: 700 } });
  });
  it("依 workflow 分組，最久的組排前面；任一筆需要注意就整組需要注意", () => {
    const gs = groupQueue([full(w("a", "fast", 100)), full(w("b", "slow", 900, { attention: true })), full(w("c", "fast", 200))]);
    expect(gs.map((g) => [g.workflow, g.attention])).toEqual([["slow", true], ["fast", false]]);
  });
  it("時間不可信的不當成最久；全部不可信才是不可信", () => {
    expect(groupQueue([full(w("a", "ci", "Untrusted")), full(w("b", "ci", 60))])[0].oldest).toEqual({ Known: { secs: 60 } });
    expect(groupQueue([full(w("a", "ci", "Untrusted"))])[0].oldest).toBe("Untrusted");
  });
  it("名冊完整卻沒有任何 runner 能接才標 noMatch；名冊不完整時不能斷言「沒有」", () => {
    const none = w("a", "ci", 60, { candidates: [] });
    expect(groupQueue([{ w: none, roster: "Full" }])[0].jobs[0].noMatch).toBe(true);
    expect(groupQueue([{ w: none, roster: "BusyOnly" }])[0].jobs[0].noMatch).toBe(false);
    expect(groupQueue([full(w("a", "ci", 60))])[0].jobs[0].noMatch).toBe(false);
  });
  it("沒有排隊＝空陣列", () => {
    expect(groupQueue([])).toEqual([]);
  });
});

describe("defaultSelected", () => {
  it("一個都沒選（剛授權完）→ 全選", () => {
    const out = defaultSelected([{ name: "A", selected: false }, { name: "B", selected: false }]);
    expect(out.map((c) => c.selected)).toEqual([true, true]);
  });
  it("已經有選擇就原樣保留，不覆蓋使用者的取捨", () => {
    const input = [{ name: "A", selected: true }, { name: "B", selected: false }];
    expect(defaultSelected(input).map((c) => c.selected)).toEqual([true, false]);
  });
  it("空陣列；且不改動輸入", () => {
    expect(defaultSelected([])).toEqual([]);
    const input = [{ name: "A", selected: false }];
    defaultSelected(input);
    expect(input[0].selected).toBe(false);
  });
});

describe("sortRunners", () => {
  it("忙碌 → 離線 → 閒置 → 名稱（離線不再排最前面），且不改動輸入", () => {
    const input = [
      runner({ name: "idle-b", state: "Idle" }),
      runner({ name: "off", state: { Offline: { since: "Unknown" } } }),
      runner({ name: "busy", state: "Busy" }),
      runner({ name: "idle-a", state: "Idle" }),
    ];
    const copy = [...input];
    expect(sortRunners(input).map((r) => r.name)).toEqual(["busy", "off", "idle-a", "idle-b"]);
    expect(input).toEqual(copy);
  });
  it("空陣列", () => {
    expect(sortRunners([])).toEqual([]);
  });
});

function snapshotWith(r: RunnerView[], waitingAge: "Untrusted" | { Known: { secs: number } } | null = null): Snapshot {
  const acct: AccountView = {
    account_id: 1,
    login: "u",
    status: "Ok",
    scopes: [
      {
        name: "o",
        kind: "Org",
        roster: "Full",
        runners: r,
        waiting: waitingAge
          ? [{ job: { ...job, state: "Queued" }, age: waitingAge, attention: false, candidates: [] }]
          : [],
        jobs: [],
        last_updated_at: NOW,
      },
    ],
  };
  return {
    schema_version: 2,
    generated_at: NOW,
    accounts: [acct],
    summary: { own_busy: 0, own_online: 0, own_offline: 0, pool_busy: 0, waiting: 0, attention: "Ok", stale: false },
  };
}

describe("hasSkew", () => {
  it("runner 或等待中任一為 Untrusted 就成立", () => {
    expect(hasSkew(snapshotWith([runner({ elapsed: "Untrusted" })]))).toBe(true);
    expect(hasSkew(snapshotWith([], "Untrusted"))).toBe(true);
  });
  it("都可信或沒有時間則不成立", () => {
    expect(hasSkew(snapshotWith([runner({ elapsed: { Known: { secs: 5 } } })], { Known: { secs: 700 } }))).toBe(false);
    expect(hasSkew(snapshotWith([]))).toBe(false);
  });
});

const POOL = { stem: "pool-8x-linux", name: "pool", machine: "8x-linux" };

describe("countVisible", () => {
  it("自家與臨時機器分開計數，排隊數照傳入的（已篩選的）數量；離線的都不算", () => {
    const rs = [
      runner({ state: "Busy" }),
      runner({ state: "Busy" }),
      runner({ state: "Idle" }),
      runner({ state: { Offline: { since: "Unknown" } } }),
      runner({ state: "Busy", pool: POOL }),
      runner({ state: { Offline: { since: "Unknown" } }, pool: POOL }),
    ];
    expect(countVisible(rs, 3)).toEqual({ ownBusy: 2, ownOnline: 3, poolBusy: 1, waiting: 3 });
  });
  it("只算傳進來的 runner：篩掉的範圍不會出現在數字裡", () => {
    expect(countVisible([], 0)).toEqual({ ownBusy: 0, ownOnline: 0, poolBusy: 0, waiting: 0 });
  });
});

describe("summaryLine / trayTitleLike", () => {
  const s = (over: Partial<Summary>): Summary => ({
    own_busy: 0,
    own_online: 0,
    own_offline: 0,
    pool_busy: 0,
    waiting: 0,
    attention: "Ok",
    stale: false,
    ...over,
  });
  it("summaryLine：沒有臨時機器在跑時省略中段，排隊永遠在最後", () => {
    expect(summaryLine(s({ own_busy: 2, own_online: 5, pool_busy: 9, waiting: 1 }))).toBe("自家 2/5 在忙 · 臨時 9 台在跑 · 排隊 1");
    expect(summaryLine(s({ own_busy: 4, own_online: 5, waiting: 7 }))).toBe("自家 4/5 在忙 · 排隊 7");
    expect(summaryLine(s({}))).toBe("自家 0/0 在忙 · 排隊 0");
  });
  it("計數文字與 Rust 端 tray_title 一致", () => {
    expect(trayTitleLike(s({ own_busy: 2, own_online: 3, waiting: 1 }))).toBe("2▶ 1⏳");
    expect(trayTitleLike(s({ own_online: 3 }))).toBe("0▶");
    expect(trayTitleLike(s({ own_busy: 1, own_online: 1, pool_busy: 2 }))).toBe("3▶");
    expect(trayTitleLike(s({}))).toBe("");
    expect(trayTitleLike(s({ own_offline: 1 }))).toBe("0▶");
  });
});

describe("dotFor / repoName / accountNote", () => {
  it("狀態點", () => {
    expect(dotFor(runner({ state: "Busy" }))).toBe("busy");
    expect(dotFor(runner({ state: "Idle" }))).toBe("idle");
    expect(dotFor(runner({ state: { Offline: { since: "Unknown" } } }))).toBe("offline");
  });
  it("repoName", () => {
    expect(repoName("Acme/acme-web")).toBe("acme-web");
    expect(repoName("plain")).toBe("plain");
  });
  it("accountNote", () => {
    expect(accountNote("Ok")).toBeNull();
    expect(accountNote("NeedsReauth")).toBe("重新登入");
    const reset = new Date(2026, 10, 2, 7, 5).getTime() / 1000;
    expect(formatClock(reset)).toBe("07:05");
    expect(accountNote({ Throttled: { remaining: 120, reset_at: reset } })).toBe("已放慢更新（剩 120 次／重置於 07:05）");
    expect(accountNote({ Offline: { last_ok_at: reset } })).toBe("最後更新 07:05");
  });
});

describe("dev-snapshot.json（?mock=1 用）", () => {
  const snap = dev as unknown as Snapshot;
  const runners = snap.accounts.flatMap((a) => a.scopes.flatMap((s) => s.runners));
  it("summary 與名冊一致，免得示意資料悄悄漂移", () => {
    const count = (f: (r: RunnerView) => boolean) => runners.filter(f).length;
    const own = (r: RunnerView) => r.pool === null;
    expect(snap.summary.own_busy).toBe(count((r) => own(r) && r.state === "Busy"));
    expect(snap.summary.own_online).toBe(count((r) => own(r) && (r.state === "Busy" || r.state === "Idle")));
    expect(snap.summary.own_offline).toBe(count((r) => own(r) && typeof r.state === "object"));
    expect(snap.summary.pool_busy).toBe(count((r) => !own(r) && r.state === "Busy"));
    expect(snap.summary.waiting).toBe(snap.accounts.flatMap((a) => a.scopes.flatMap((s) => s.waiting)).length);
  });
  it("排序：忙碌的在前，離線不再排最前面", () => {
    expect(sortRunners(runners)[0].state).toBe("Busy");
  });
});

describe("runner 執行歷史", () => {
  const rec = (over: Partial<JobRecord> = {}): JobRecord => ({
    job_id: 1,
    run_id: 1,
    repo: "o/r",
    workflow: "ci",
    job: "build",
    pr_tag: "PR#1",
    url: "",
    started_at: NOW - 600,
    completed_at: NOW - 60,
    outcome: "Success",
    source: "Observed",
    ...over,
  });
  it("結果標籤：失敗與逾時是紅、取消與略過是灰、執行中是藍", () => {
    expect(outcomeLabel("Success")).toEqual({ text: "成功", tone: "ok" });
    expect(outcomeLabel("Failure").tone).toBe("bad");
    expect(outcomeLabel("TimedOut")).toEqual({ text: "逾時", tone: "bad" });
    expect(outcomeLabel("Cancelled").tone).toBe("muted");
    expect(outcomeLabel("Running")).toEqual({ text: "執行中", tone: "run" });
  });
  it("跑了多久：已結束用結束時間，還在跑用現在，沒有開始時間就空白", () => {
    expect(recordDuration(rec(), NOW)).toBe("9m");
    expect(recordDuration(rec({ outcome: "Running", completed_at: null, started_at: NOW - 120 }), NOW)).toBe("2m");
    expect(recordDuration(rec({ started_at: null }), NOW)).toBe("");
  });
  it("時間：今天、昨天、更早用月／日", () => {
    const base = new Date(2026, 10, 2, 21, 30).getTime() / 1000;
    expect(recordWhen(base - 30 * 60, base)).toBe("今天 21:00");
    expect(recordWhen(new Date(2026, 10, 1, 8, 5).getTime() / 1000, base)).toBe("昨天 08:05");
    expect(recordWhen(new Date(2026, 9, 28, 23, 59).getTime() / 1000, base)).toBe("10/28 23:59");
    expect(recordWhen(null, base)).toBe("時間不明");
  });
  it("篩選與計數：失敗含逾時，其他含取消、略過與執行中", () => {
    const rs = [
      rec({ job_id: 1 }),
      rec({ job_id: 2, outcome: "Failure" }),
      rec({ job_id: 3, outcome: "TimedOut" }),
      rec({ job_id: 4, outcome: "Cancelled" }),
      rec({ job_id: 5, outcome: "Running" }),
    ];
    expect(filterRecords(rs, "bad").map((r) => r.job_id)).toEqual([2, 3]);
    expect(filterRecords(rs, "other").map((r) => r.job_id)).toEqual([4, 5]);
    expect(countRecords(rs)).toEqual({ all: 5, ok: 1, bad: 2, other: 2 });
  });
  it("補查說明：沒有可查的 repo、部分查不到", () => {
    expect(backfillNote({ repos_checked: 0, runs_checked: 0, added: 0, partial: false })).toBe(
      "最近 7 天沒有推送過的 repo，沒有可以補查的紀錄",
    );
    expect(backfillNote({ repos_checked: 2, runs_checked: 16, added: 5, partial: false })).toBe("查了 2 個 repo、16 次執行，補到 5 筆");
    expect(backfillNote({ repos_checked: 1, runs_checked: 8, added: 0, partial: true })).toContain("有些 repo 查不到");
  });
  it("findRunner：依 key 找到 runner 與它的帳號、範圍", () => {
    const snap = snapshotWith([runner({ key: "1/o/9", name: "builder" })]);
    expect(findRunner(snap, "1/o/9")).toMatchObject({ accountId: 1, scope: "o", runner: { name: "builder" } });
    expect(findRunner(snap, "nope")).toBeNull();
  });
});

describe("jobWhere", () => {
  it("臨時機器（只寫組名與機型）、GitHub-hosted、自家 runner 三者分開，臨時機器先判斷", () => {
    expect(jobWhere({ pool: POOL, hosted: false, runner_name: "pool-8x-linux-q7k2m9x4b1c8d3e5" })).toBe("pool 8x-linux");
    expect(jobWhere({ pool: { stem: "pool", name: "pool", machine: "" }, hosted: false, runner_name: null })).toBe("pool");
    expect(jobWhere({ pool: null, hosted: true, runner_name: "GitHub Actions 3" })).toBe("GitHub-hosted");
    expect(jobWhere({ pool: null, hosted: false, runner_name: "netcup-debian13-vps" })).toBe("netcup-debian13-vps");
  });
  it("排隊中、還沒有 runner 名稱時不顯示", () => {
    expect(jobWhere({ pool: null, hosted: false, runner_name: null })).toBe("");
  });
});

describe("splitRunners", () => {
  const poolJob = (name: string, over: Partial<JobView> = {}): JobView => ({ ...job, job: name, pool: POOL, hosted: false, ...over });
  const elapsed = (secs: number) => ({ Known: { secs } });
  const OFF = { Offline: { since: "Unknown" as const } };
  it("自家機器依狀態分三堆（依名稱排序）；臨時機器歸組、離線殘骸完全丟掉", () => {
    const rs = [
      runner({ name: "own-busy-b", state: "Busy" }),
      runner({ name: "own-busy-a", state: "Busy" }),
      runner({ name: "own-idle", state: "Idle" }),
      runner({ name: "own-off", state: OFF }),
      runner({ name: "pool-1", state: "Busy", pool: POOL, job: poolJob("build"), elapsed: elapsed(60) }),
      runner({ name: "pool-2", state: "Busy", pool: POOL, job: poolJob("build"), elapsed: elapsed(300) }),
      runner({ name: "pool-3", state: "Idle", pool: POOL }),
      runner({ name: "pool-4", state: OFF, pool: POOL }),
      // 另一組：只剩離線殘骸 → 整組不出現
      runner({ name: "gone-1", state: OFF, pool: { stem: "gone-2x", name: "gone", machine: "2x" } }),
    ];
    const s = splitRunners(rs);
    expect(s.busy.map((r) => r.name)).toEqual(["own-busy-a", "own-busy-b"]);
    expect(s.idle.map((r) => r.name)).toEqual(["own-idle"]);
    expect(s.offline.map((r) => r.name)).toEqual(["own-off"]);
    expect(s.pools).toHaveLength(1);
    expect(s.pools[0]).toMatchObject({ tag: POOL, busy: 2, idle: 1 });
  });
  it("同 repo＋job 名稱合併成 count，elapsed 取最久的；只有一個時才有連結", () => {
    const rs = [1, 2, 3].map((i) =>
      runner({ name: `p${i}`, state: "Busy", pool: POOL, job: poolJob("build"), elapsed: elapsed(i * 100) }),
    ).concat([
      runner({ name: "p4", state: "Busy", pool: POOL, job: poolJob("lint", { url: "https://github.com/lint" }), elapsed: elapsed(30) }),
    ]);
    const [g] = splitRunners(rs).pools;
    expect(g.jobs.map((j) => [j.job.job, j.count, j.url])).toEqual([
      ["build", 3, null],
      ["lint", 1, "https://github.com/lint"],
    ]);
    expect(g.jobs[0].elapsed).toEqual(elapsed(300));
  });
  it("沒有任何機器", () => {
    expect(splitRunners([])).toEqual({ busy: [], idle: [], offline: [], pools: [] });
  });
});

describe("waitLabel", () => {
  it("等臨時機器：組名＋機型；機型空時只寫組名；否則等自家機器", () => {
    expect(waitLabel({ ...job, pool: POOL })).toBe("等 pool 8x-linux");
    expect(waitLabel({ ...job, pool: { stem: "pool", name: "pool", machine: "" } })).toBe("等 pool");
    expect(waitLabel({ ...job, pool: null })).toBe("等自家機器");
  });
  it("groupQueue：等臨時機器的 job 不算「沒有 runner 能接」，並帶上等什麼", () => {
    const pooled: WaitingView = {
      job: { ...job, state: "Queued", pool: POOL, job: "e2e", workflow: "ci", key: "p/1" },
      age: { Known: { secs: 700 } },
      attention: true,
      candidates: [],
    };
    const own: WaitingView = { ...pooled, job: { ...pooled.job, pool: null, job: "mac", key: "p/2" } };
    const [g] = groupQueue([
      { w: pooled, roster: "Full" },
      { w: own, roster: "Full" },
    ]);
    expect(g.jobs.map((j) => [j.name, j.noMatch, j.wait])).toEqual([
      ["e2e", false, "等 pool 8x-linux"],
      ["mac", true, "等自家機器"],
    ]);
  });
  it("groupQueue：同名但等不同機器的 job 不合併成同一個「×n」", () => {
    const mk = (key: string, pool: typeof POOL | null): WaitingView => ({
      job: { ...job, state: "Queued", pool, job: "e2e", workflow: "ci", key },
      age: { Known: { secs: 100 } },
      attention: false,
      candidates: [],
    });
    const big = { stem: "pool-16x-linux", name: "pool", machine: "16x-linux" };
    const [g] = groupQueue([
      { w: mk("a", POOL), roster: "Full" },
      { w: mk("b", big), roster: "Full" },
      { w: mk("c", POOL), roster: "Full" },
    ]);
    expect(g.jobs.map((j) => [j.count, j.wait])).toEqual([
      [2, "等 pool 8x-linux"],
      [1, "等 pool 16x-linux"],
    ]);
  });
});

describe("detectPlatform / isDesktop / isMobile", () => {
  it("優先用 Tauri 的作業系統名稱", () => {
    expect(detectPlatform("ios", "")).toBe("ios");
    expect(detectPlatform("android", "")).toBe("android");
    expect(detectPlatform("windows", "")).toBe("windows");
    expect(detectPlatform("macos", "")).toBe("mac");
    expect(detectPlatform("linux", "")).toBe("mac");
  });
  it("沒有作業系統名稱時看 userAgent（瀏覽器預覽）", () => {
    expect(detectPlatform(null, "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)")).toBe("ios");
    expect(detectPlatform(null, "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X)")).toBe("ios");
    expect(detectPlatform(null, "Mozilla/5.0 (Linux; Android 15)")).toBe("android");
    expect(detectPlatform(null, "Mozilla/5.0 (Windows NT 10.0)")).toBe("windows");
    expect(detectPlatform(null, "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("mac");
  });
  it("行動版（android、ios）不是桌面版", () => {
    expect(isDesktop("mac")).toBe(true);
    expect(isDesktop("windows")).toBe(true);
    expect(isDesktop("android")).toBe(false);
    expect(isDesktop("ios")).toBe(false);
    expect(isMobile("ios")).toBe(true);
    expect(isMobile("android")).toBe(true);
    expect(isMobile("mac")).toBe(false);
  });
});
