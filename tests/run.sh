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
expect_has "統計" "自家 2 台：🟢1 忙碌  ⚪1 閒置 · 臨時 2 在跑"
expect_has "忙碌 runner 帶 repo/PR/job/時間" "runner-a               ← [repo-x PR#42] CI → build (2m5s)"
expect_has "閒置 runner" "runner-b               閒置"
expect_has "自家離線收成最後一行" "離線：runner-c"
expect_has "臨時機器依組鍵歸成一行，寫組名與機型" "pool 8x-linux"
expect_has "組內忙碌台數" "2 在跑"
expect_has "組內 job 摘要" "CI → e2e"
expect_not "沒有時間偏移註記" '⚠\?'
expect_not "任何亂數 runner 名稱（在線或離線殘骸）都不出現" 'q7k2m9x4b1c8d3e5|z8y7x6w5v4u3t2s1|a1b2c3d4e5f6g7h8|m3n4o5p6q7r8s9t0|abcdefghijklmnop'
expect_not "離線的臨時機器殘骸不算離線 runner" '離線：.*pool'

echo "--runners：名冊裡只剩臨時機器的離線殘骸（不是缺權限）"
STUB_ONLY_VMS=1 run --runners --repo acme
expect_code "exit" 0
expect_not "不把它誤報成缺 admin:org" 'admin:org'
expect_has "統計是自家 0 台" "自家 0 台"
expect_has "說明沒有自家 runner" "目前沒有任何自家 runner"
expect_not "沒有臨時機器在跑就不出現那一段" '臨時'
expect_not "殘骸不被列成離線 runner" '離線：'
expect_not "殘骸名稱不出現" 'a1b2c3d4e5f6g7h8|m3n4o5p6q7r8s9t0|abcdefghijklmnop'

echo "--runners：名稱同前綴、尾巴不像亂數的自家 runner 仍是自家 runner"
STUB_LOOKALIKE=1 run --runners --repo acme
expect_code "exit" 0
expect_has "blacksmith-builder 是自家 runner，照常列出" "blacksmith-builder"
expect_has "它也算進統計" "自家 3 台"

echo "--runners：案例表（tests/fixtures/runner-names.tsv，與 Rust 單元測試共用）"
NAMES_TSV="$ROOT/tests/fixtures/runner-names.tsv"
SCENARIO_TMP="$(mktemp -d)"
trap 'rm -rf "$SCENARIO_TMP"' EXIT
# labels- 開頭的情境要 job label（規則 c），終端機工具沒有抓，跳過
scenarios=$(awk -F'\t' '/^#/ || NF < 5 {next} $1 !~ /^labels-/ {print $1}' "$NAMES_TSV" | sort -u)
for sc in $scenarios; do
  awk -F'\t' -v sc="$sc" '$1 == sc && $2 == "runner" {print $3}' "$NAMES_TSV" \
    | jq -R '{name: ., status: "online", busy: false, labels: []}' | jq -s '{runners: .}' > "$SCENARIO_TMP/$sc.json"
  STUB_RUNNERS_JSON="$SCENARIO_TMP/$sc.json" run --runners --repo acme
  expect_code "[$sc] exit" 0
  while IFS=$'\t' read -r _ _ value expect stem; do
    if [ "$expect" = own ]; then
      expect_has "[$sc] 自家機器 $value 照常列出" "$value"
    else
      expect_not "[$sc] 臨時機器 $value 的名稱不出現" "$value"
      disp="${stem%%-*}"; [ "$stem" = "$disp" ] || disp="$disp ${stem#*-}"
      expect_has "[$sc] 組 $disp 出現" "$disp"
    fi
  done < <(awk -F'\t' -v sc="$sc" '$1 == sc && $2 == "runner"' "$NAMES_TSV")
done

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
expect_has "忙碌清單（沒有名冊的路徑）臨時機器也歸組" "pool 8x-linux"
expect_not "忙碌清單不列出亂數 runner 名稱" 'q7k2m9x4b1c8d3e5|z8y7x6w5v4u3t2s1'

echo "PR 檢視（--local）"
STUB_START_OFFSET=-125 run 42 --repo acme/repo-x --local
expect_code "單次模式 exit" 0
expect_has "PR 標頭" "PR #42"
expect_has "進行中區塊" "▶ 進行中 / 排隊"
expect_has "self-hosted job 顯示 runner" "runner-a"
expect_has "hosted job 標示" "☁️ hosted"
expect_has "臨時機器 job 只標組名與機型" "☁ pool 8x-linux"
expect_not "臨時機器 job 不顯示亂數 runner 名稱" 'q7k2m9x4b1c8d3e5|z8y7x6w5v4u3t2s1'
expect_not "臨時機器不進 runner 名冊" '(🟢|⚪) +pool'
# 只看 integration 那一列：全域比對會被另一個臨時機器 job（e2e）湊巧滿足
FULL_OUT="$OUT"; OUT=$(grep -F 'integration' <<<"$FULL_OUT" || true)
expect_has "integration 那一列標的是臨時機器" "☁ pool 8x-linux"
expect_not "integration 那一列不被當成 hosted" '☁️ hosted'
OUT="$FULL_OUT"
expect_has "已完成 job 時間" "1m5s"

echo "PR 檢視（跨 repo 掃描）：臨時機器不進 runner 名冊"
STUB_START_OFFSET=-125 run 42 --repo acme/repo-x
expect_code "跨 repo 模式 exit" 0
expect_has "自家 runner 照常列出" "runner-a"
expect_not "亂數 runner 名稱不出現（忙碌快照那條路徑）" 'q7k2m9x4b1c8d3e5|z8y7x6w5v4u3t2s1'
expect_not "臨時機器不被列成 runner" '(🟢|⚪) +pool'

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
