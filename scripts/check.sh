#!/usr/bin/env bash
# 提交前檢查：clippy（警告視為錯誤）+ 全部測試。任一失敗即以非零結束。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
