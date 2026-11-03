#!/usr/bin/env bash
# ci:hold 的判斷邏輯（約定定義：enabling-ci-hold）。ci.yml 呼叫，tests/run.sh 也測它。
#   ci-hold.sh held   讀 LABELS_JSON、DRAFT，印出 true / false
#   ci-hold.sh gate   讀 HELD、TEST_RESULT，不該通過時 exit 1
#
# 設計：未知一律往「照常跑」走——標籤資料缺漏、不是陣列、解析失敗都當作沒 hold，
# 免得壞資料把整個 PR 卡住。反過來，gate 只認 test 的結果是 success：
# skipped 不能讀成通過，否則貼著 hold 的 ready PR 會變成可以 merge。
set -uo pipefail

LABEL="ci:hold"

held() {
  # draft 的行為不變：只有 ready 的 PR 才會被 hold。
  [ "${DRAFT:-}" = "true" ] && { echo false; return; }
  if [ "$(jq -r --arg l "$LABEL" 'if type == "array" and (map(select(. == $l)) | length) > 0 then "true" else "false" end' <<<"${LABELS_JSON:-}" 2>/dev/null)" = "true" ]; then
    echo true
  else
    echo false
  fi
}

gate() {
  if [ "${HELD:-}" = "true" ]; then
    echo "::error title=ci:hold::PR 貼著 ${LABEL}，重活 CI 已略過。這個檢查失敗是設計：移除標籤才會跑一次完整 CI。"
    echo "ci:hold 生效中：移除 ${LABEL} 標籤後才會跑完整 CI。" >&2
    return 1
  fi
  if [ "${TEST_RESULT:-}" != "success" ]; then
    echo "::error title=test::test job 結果是「${TEST_RESULT:-空}」，不是 success。" >&2
    return 1
  fi
}

case "${1:-}" in
  held) held ;;
  gate) gate ;;
  *) echo "用法：ci-hold.sh held|gate" >&2; exit 2 ;;
esac
