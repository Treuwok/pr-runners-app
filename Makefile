.PHONY: test lint rust install check check-app
# rules 與 core 是 CI 會跑的 Rust crate；Tauri App crate 放在 check-app（只在 Mac 本機跑）。
RUST_CRATES := -p pr-runners-rules -p pr-runners-core

check: lint test rust

lint:
	shellcheck bin/pr-runners install.sh tests/run.sh tests/stub/gh .github/scripts/ci-hold.sh scripts/dev-sign-run.sh

test:
	tests/run.sh

rust:
	cargo test $(RUST_CRATES)
	cargo clippy $(RUST_CRATES) --all-targets -- -D warnings
	cargo build -p pr-runners-rules --target wasm32-unknown-unknown
	@# 上面的 cargo test 會重新匯出 ts-rs 型別；匯出結果必須和 commit 的一致（含新增的檔案）
	git diff --exit-code -- app/src/lib/generated
	@test -z "$$(git ls-files --others --exclude-standard -- app/src/lib/generated)" || { echo "app/src/lib/generated 有未追蹤的新檔案，請 commit"; exit 1; }

# Tauri App crate、前端單元測試與型別檢查。只在 Mac 本機跑：
# CI 機器沒有 webkit2gtk，也沒有前端產物（`generate_context!` 編不過）。
check-app:
	cd app && pnpm build
	cargo test -p pr-runners-app
	cargo clippy -p pr-runners-app --all-targets -- -D warnings
	git diff --exit-code -- app/src/lib/generated
	@test -z "$$(git ls-files --others --exclude-standard -- app/src/lib/generated)" || { echo "app/src/lib/generated 有未追蹤的新檔案，請 commit"; exit 1; }
	cd app && pnpm test
	cd app && pnpm run check

install:
	./install.sh
