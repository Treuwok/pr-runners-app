#!/usr/bin/env bash
# 把 bin/pr-runners 以 symlink 裝進 PATH 目錄（預設 ~/bin）。之後 git pull 就是更新。
#   ./install.sh                # → ~/bin/pr-runners
#   PREFIX=/usr/local/bin ./install.sh
set -euo pipefail
src="$(cd "$(dirname "$0")" && pwd)/bin/pr-runners"
dir="${PREFIX:-$HOME/bin}"; dst="$dir/pr-runners"
mkdir -p "$dir"

if [ -L "$dst" ] && [ "$(readlink "$dst")" = "$src" ]; then
  echo "已安裝：$dst → $src"; exit 0
fi
# 既有的一般檔（例如以前手動 cp 的舊版）先備份，不直接覆蓋。
# 用 mv/ln（換目錄項）而不是改寫原檔：正在跑的 `pr-runners --watch` 還開著舊 inode，不受影響。
if [ -e "$dst" ] && [ ! -L "$dst" ]; then
  bak="$dst.bak-$(date +%Y%m%d%H%M%S)"; mv "$dst" "$bak"; echo "備份舊檔：$bak"
fi
ln -sfn "$src" "$dst"
echo "已安裝：$dst → $src"
case ":$PATH:" in *":$dir:"*) ;; *) echo "注意：$dir 不在 PATH 裡，請加進 shell 設定檔。";; esac
