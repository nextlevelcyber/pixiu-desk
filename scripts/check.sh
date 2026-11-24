#!/usr/bin/env bash
# 本仓库的检查门（没有 CI，推送前在本地跑）：工具链、格式、clippy、测试。
set -euo pipefail
cd "$(dirname "$0")/.."
ver=$(rustc --version | awk '{print $2}')
case "$ver" in
    1.97.*) ;;
    *) echo "check：需要 rustc 1.97.x，当前 $ver（不要自行安装或更换工具链）" >&2; exit 1 ;;
esac
echo "== cargo fmt"; cargo fmt --all --check
echo "== cargo clippy"; cargo clippy --all-targets --locked -- -D warnings
echo "== cargo test"; cargo test --locked
echo "check：全部通过"
