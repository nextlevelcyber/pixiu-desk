# 测试夹具

| 文件 | 来源 | 读取它的测试 |
|---|---|---|
| `bt-btc-2h/journal.jsonl` | Pixiu `pixiu-backtest --fill-model trades`（Pixiu `974f1f7`），输入为研究桶 `capture/20261028/` 的 `binance-md-20261028T01-000`、`T02-000` 和 `bitget-public-20261028T01-000`、`T02-000`，配置为本地开发用的 BTC 示例值（edge 2 bps，两档各 0.0002 BTC，不是实盘配置）；journal 只保留 `run_start`、`run_stop`、`mode`、`order_registered`、`fill`、`order_fee`、`booked`、`position_snapshot` 以及首尾两行 | `tests/golden.rs` |
| `bt-btc-2h/basis-BTCUSDT.tsv` | 同一次回测的 basis 采样（1 秒一行） | `tests/golden.rs` |
| `bt-btc-2h/summary.json` | 同一次回测的 summary | `tests/golden.rs` |
| `bt-btc-2h/live_report.json` | `python3 scripts/lib/live_report.py --journal bt-btc-2h/journal.jsonl --basis bt-btc-2h/basis-BTCUSDT.tsv --instruments BTCUSDT --json ...`（Pixiu `974f1f7`），在裁剪后的 journal 上生成 | `tests/golden.rs` |
