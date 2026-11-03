#!/usr/bin/env bash
# 開發用：每次編譯後用固定的開發者憑證重新簽章再執行。
# ad-hoc 簽章每次編譯都不同，macOS 鑰匙圈會把它當成新程式、每次都要密碼；固定簽章後按一次「永遠允許」即可。
# 用法：CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER=scripts/dev-sign-run.sh PR_RUNNERS_DEV_SIGN_IDENTITY=<SHA-1> pnpm tauri dev
set -euo pipefail
bin="$1"
if [ -n "${PR_RUNNERS_DEV_SIGN_IDENTITY:-}" ]; then
  codesign --force --sign "$PR_RUNNERS_DEV_SIGN_IDENTITY" --identifier dev.pr-runners.app "$bin" >&2
fi
exec "$@"
