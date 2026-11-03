import { describe, expect, it } from "vitest";
import dev from "./dev-snapshot.json";
import type { AccountView } from "./generated/AccountView";
import type { RunnerView } from "./generated/RunnerView";
import type { Snapshot } from "./generated/Snapshot";
import type { JobRecord } from "./generated/JobRecord";
import type { WaitingView } from "./generated/WaitingView";
import type { Summary } from "./generated/Summary";
import {
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
    attention: false,
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
  it("需要注意 → 忙碌 → 離線 → 閒置 → 名稱，且不改動輸入", () => {
    const input = [
      runner({ name: "idle-b", state: "Idle" }),
      runner({ name: "off", state: { Offline: { since: "Unknown" } } }),
      runner({ name: "busy", state: "Busy" }),
      runner({ name: "idle-a", state: "Idle" }),
      runner({ name: "bad", state: { Offline: { since: "Unknown" } }, attention: true }),
    ];
    const copy = [...input];
    expect(sortRunners(input).map((r) => r.name)).toEqual(["bad", "busy", "off", "idle-a", "idle-b"]);
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
    schema_version: 1,
    generated_at: NOW,
    accounts: [acct],
    summary: { busy: 0, idle: 0, offline: 0, waiting: 0, attention: "Ok" },
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

describe("countVisible", () => {
  it("依狀態分別計數，排隊數照傳入的（已篩選的）數量", () => {
    const rs = [
      runner({ state: "Busy" }),
      runner({ state: "Busy" }),
      runner({ state: "Idle" }),
      runner({ state: { Offline: { since: "Unknown" } } }),
    ];
    expect(countVisible(rs, 3)).toEqual({ busy: 2, idle: 1, offline: 1, waiting: 3, attention: "Ok" });
  });
  it("只算傳進來的 runner：篩掉的範圍不會出現在數字裡", () => {
    expect(countVisible([], 0)).toEqual({ busy: 0, idle: 0, offline: 0, waiting: 0, attention: "Ok" });
  });
  it("有需要注意的 runner 才標 Critical", () => {
    const bad = runner({ state: { Offline: { since: "Unknown" } }, attention: true });
    expect(countVisible([bad], 0).attention).toBe("Critical");
  });
});

describe("summaryLine / trayTitleLike", () => {
  const s = (over: Partial<Summary>): Summary => ({ busy: 0, idle: 0, offline: 0, waiting: 0, attention: "Ok", ...over });
  it("summaryLine：離線只在有的時候出現，排隊永遠在最後", () => {
    expect(summaryLine(s({ busy: 2, idle: 2, offline: 1, waiting: 7 }))).toBe("2 忙碌 · 2 閒置 · 1 離線 · 7 排隊");
    expect(summaryLine(s({ busy: 4, idle: 1, waiting: 7 }))).toBe("4 忙碌 · 1 閒置 · 7 排隊");
    expect(summaryLine(s({}))).toBe("0 忙碌 · 0 閒置 · 0 排隊");
  });
  it("計數文字與 Rust 端 tray_title 一致", () => {
    expect(trayTitleLike(s({ busy: 2, idle: 1, waiting: 1 }))).toBe("2▶ 1⏳");
    expect(trayTitleLike(s({ idle: 3 }))).toBe("0▶");
    expect(trayTitleLike(s({}))).toBe("");
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
    expect(snap.summary.busy).toBe(count((r) => r.state === "Busy"));
    expect(snap.summary.idle).toBe(count((r) => r.state === "Idle"));
    expect(snap.summary.offline).toBe(count((r) => typeof r.state === "object"));
    expect(snap.summary.waiting).toBe(snap.accounts.flatMap((a) => a.scopes.flatMap((s) => s.waiting)).length);
  });
  it("排序把需要注意的離線 runner 放最前面", () => {
    expect(sortRunners(runners)[0].name).toBe("vps-2");
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
