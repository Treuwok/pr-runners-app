#!/usr/bin/env bash
# 離線測試：用 tests/stub/gh 取代真的 gh，不打 GitHub API。
#   tests/run.sh                 # 用 PATH 上的 bash 跑
#   BASH_BIN=/bin/bash tests/run.sh   # 指定 bash（例如 macOS 內建 3.2）
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASH_BIN="${BASH_BIN:-bash}"
export PATH="$ROOT/tests/stub:$PATH"
export TERM=dumb
unset PR_RUNNERS_ORG PR_RUNNERS_INTERVAL

pass=0; fail=0
OUT=""; CODE=0
ESC=$(printf '\033')
run() {
  OUT=$("$BASH_BIN" "$ROOT/bin/pr-runners" "$@" 2>&1); CODE=$?
  # shellcheck disable=SC2001  # 要 regex（ANSI 色碼），參數展開做不到
  OUT=$(sed "s/${ESC}\[[0-9;]*m//g" <<<"$OUT")   # 比對前去掉色碼（org=\033[36macme 這類）
}
ok()  { pass=$((pass + 1)); printf '  ✓ %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf '  ✗ %s\n' "$1"; while IFS= read -r l; do printf '      | %s\n' "$l"; done <<<"$OUT"; }
expect_code() { if [ "$CODE" = "$2" ]; then ok "$1（exit $2）"; else bad "$1：預期 exit $2，實際 $CODE"; fi; }
expect_has()  { if grep -qF -- "$2" <<<"$OUT"; then ok "$1"; else bad "$1：輸出缺少「$2」"; fi; }
expect_not()  { if grep -qE -- "$2" <<<"$OUT"; then bad "$1：輸出不該出現 /$2/"; else ok "$1"; fi; }

# shellcheck disable=SC2016  # $BASH_VERSION 要在被測的 bash 裡展開
echo "bash: $("$BASH_BIN" -c 'echo "$BASH_VERSION"')"

echo "參數解析"
run --help;                 expect_code "--help" 0; expect_has "--help 印出用法" "pr-runners <PR 編號>"
expect_not "--help 不帶註解井字號" '^#'
run;                        expect_code "沒給參數" 2
run --repo;                 expect_code "--repo 缺值" 2;  expect_has "--repo 缺值訊息" "--repo 需要一個值"
run --repo --watch 1;       expect_code "--repo 後面接旗標" 2
run --bogus 1;              expect_code "未知選項" 2;     expect_has "未知選項訊息" "不認得的選項"
run abc;                    expect_code "PR 非數字" 2
run 1 2;                    expect_code "兩個 PR 編號" 2
run --runners 5;            expect_code "--runners 帶 PR" 2
run --runners --local --repo acme; expect_code "--runners 配 --local" 2
PR_RUNNERS_INTERVAL=0 run --runners --repo acme;  expect_code "INTERVAL=0" 2
run --runners;              expect_code "--runners 找不到 org" 2; expect_has "找不到 org 的提示" "PR_RUNNERS_ORG"

echo "--runners：完整名冊（時間正常）"
STUB_START_OFFSET=-125 run --runners --repo acme
expect_code "exit" 0
expect_has "標頭" "org=acme"
expect_has "統計" "共 3 台：🟢1 忙碌  ⚪1 閒置  🔴1 離線"
expect_has "忙碌 runner 帶 repo/PR/job/時間" "runner-a               ← [repo-x PR#42] CI → build (2m5s)"
expect_has "閒置 runner" "runner-b               閒置"
expect_has "離線 runner" "runner-c               離線"
expect_not "沒有時間偏移註記" '⚠\?'
expect_not "Blacksmith 的雲端 VM（名冊裡離線）不算 runner" 'blacksmith-2vcpu'
expect_not "同時帶 self-hosted 與 Blacksmith 機型標籤的 job 也不是自家 runner" 'blacksmith-8vcpu'

echo "--runners：名冊裡只有 Blacksmith 的雲端 VM（不是缺權限）"
STUB_ONLY_VMS=1 run --runners --repo acme
expect_code "exit" 0
expect_not "不把它誤報成缺 admin:org" 'admin:org'
expect_has "統計是 0 台" "共 0 台"
expect_not "VM 不被當成 runner 列出" 'blacksmith-2vcpu'

echo "--runners：名稱同前綴的自家 runner 仍在名冊裡"
STUB_LOOKALIKE=1 run --runners --repo acme
expect_code "exit" 0
expect_has "blacksmith-builder 是自家 runner，照常列出" "blacksmith-builder"
expect_has "它也算進統計" "共 4 台"

echo "--runners：GitHub 時間戳在未來（2026-11-01 實際發生）"
STUB_START_OFFSET=3005 run --runners --repo acme
expect_code "exit" 0
expect_has "已跑時間改標 ⚠?" "CI → build (⚠?)"
expect_has "底部加註原因" "GitHub API 回傳的開始時間在未來"
expect_not "不再印出負數時間" '\(-[0-9]+m'

echo "--runners：沒有 admin:org（runners API 403）"
STUB_NO_ADMIN=1 run --runners --repo acme
expect_code "exit" 0
expect_has "退回只顯示忙碌" "🟢 runner-a"
expect_has "授權提示" "gh auth refresh -h github.com -s admin:org"
expect_not "不把 403 的 JSON 當成 runner 名" 'Must have admin'
expect_not "忙碌清單（沒有名冊的路徑）也不列出 Blacksmith 的雲端 VM" 'blacksmith-8vcpu'

echo "PR 檢視（--local）"
STUB_START_OFFSET=-125 run 42 --repo acme/repo-x --local
expect_code "單次模式 exit" 0
expect_has "PR 標頭" "PR #42"
expect_has "進行中區塊" "▶ 進行中 / 排隊"
expect_has "self-hosted job 顯示 runner" "runner-a"
expect_has "hosted job 標示" "☁️ hosted"
expect_has "Blacksmith job 單獨標示" "☁ Blacksmith"
expect_not "Blacksmith job 不進 runner 名冊" '(🟢|⚪) +blacksmith'
# 只看 integration 那一列：全域比對會被另一個 Blacksmith job（e2e）湊巧滿足
FULL_OUT="$OUT"; OUT=$(grep -F 'integration' <<<"$FULL_OUT" || true)
expect_has "integration 那一列標的是 Blacksmith" "☁ Blacksmith"
expect_not "integration 那一列不被當成 hosted" '☁️ hosted'
OUT="$FULL_OUT"
expect_has "已完成 job 時間" "1m5s"

echo "PR 檢視：時間戳在未來"
STUB_START_OFFSET=3005 run 42 --repo acme/repo-x --local
expect_has "進行中 job 標 ⚠?" "⚠?"
expect_has "底部加註" "GitHub API 回傳的開始時間在未來"

echo "ci:hold 判斷（.github/scripts/ci-hold.sh）"
HOLD_SH="$ROOT/.github/scripts/ci-hold.sh"
hold() {   # hold <held|gate> [VAR=值 ...]：用乾淨環境跑，只帶指定的變數
  local mode="$1"; shift
  OUT=$(env -i PATH="$PATH" "$@" "$BASH_BIN" "$HOLD_SH" "$mode" 2>&1); CODE=$?
}
expect_out()  { if [ "$OUT" = "$2" ]; then ok "$1"; else bad "$1：預期輸出「$2」"; fi; }
hold held 'LABELS_JSON=["ci:hold"]' DRAFT=false;            expect_out "ready＋貼著 ci:hold → hold" true
hold held 'LABELS_JSON=["bug","ci:hold"]' DRAFT=false;      expect_out "多個標籤中有 ci:hold → hold" true
hold held 'LABELS_JSON=["ci:hold"]' DRAFT=true;             expect_out "draft 貼著 ci:hold → 不 hold（draft 行為不變）" false
hold held 'LABELS_JSON=["bug"]' DRAFT=false;                expect_out "沒有 ci:hold → 不 hold" false
hold held 'LABELS_JSON=["ci:hold-not"]' DRAFT=false;        expect_out "名稱只是前綴相同 → 不 hold" false
hold held 'LABELS_JSON=[]' DRAFT=false;                     expect_out "空標籤 → 不 hold" false
hold held 'LABELS_JSON=null' DRAFT=;                        expect_out "push 事件（標籤是 null）→ 不 hold" false
hold held 'LABELS_JSON="ci:hold"' DRAFT=false;              expect_out "標籤不是陣列 → 當作沒 hold" false
hold held 'LABELS_JSON={"name":"ci:hold"}' DRAFT=false;     expect_out "標籤是物件 → 當作沒 hold" false
hold held 'LABELS_JSON=not json' DRAFT=false;               expect_out "標籤解析失敗 → 當作沒 hold" false
hold held DRAFT=false;                                      expect_out "標籤資料缺漏 → 當作沒 hold" false
# gate：hold 時必須失敗，而且不能因為 test 被略過（skipped）就通過。
hold gate HELD=true TEST_RESULT=skipped;  expect_code "hold＋test skipped → check 失敗" 1; expect_has "失敗訊息含 ci:hold" "ci:hold"
hold gate HELD=true TEST_RESULT=success;  expect_code "hold 時即使 test success 也失敗" 1
hold gate HELD=false TEST_RESULT=success; expect_code "沒 hold＋test success → 通過" 0
hold gate HELD=false TEST_RESULT=skipped; expect_code "沒 hold 但 test 被略過 → 失敗（skipped 不是通過）" 1
hold gate HELD=false TEST_RESULT=failure; expect_code "test failure → 失敗" 1
hold gate HELD= TEST_RESULT=skipped;      expect_code "hold job 沒輸出且 test 被略過 → 失敗" 1
hold gate HELD= TEST_RESULT=success;      expect_code "hold 輸出缺漏但 test success → 照常通過" 0
hold bogus;                               expect_code "未知子命令" 2

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
