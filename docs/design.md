# Pixiu Desk 设计文档 v0

- 日期：2026-11-24
- 状态：草案，待你确认
- 决策依据：Pixiu 仓库 `docs/plans/handoff/2026-11-24-OWNER-desk-console.md`

## 1. 目标

只给你个人用的交易后台，一个 Rust 进程、一个网页，覆盖三块：

1. **控制台（只读）**：运行状态、模式、hold、feed 健康、持仓、在场单、告警。
2. **盈亏分析**：实盘盈亏曲线、五项拆分（成交、库存、手续费、资金费、体系外）、**markout**、数据核对。
3. **回测结果**：回测运行列表、单次结果（累计曲线、成交、markout）、多次对比、实盘与回测叠加。

### 不做的事

- 不下单，不撤单，不暂停（第一版）。`pixiu ctl` 继续通过 SSM 执行。
- 不改 Pixiu 二进制，不给它加任何接口。desk 只读 Pixiu 已经产出的东西。
- 不在 desk 里实现策略或回测引擎。回测始终由 Pixiu 的 `pixiu-backtest` 来跑。
- 不做多用户和权限体系。网页只监听 loopback，通过 SSM 端口转发访问。

## 2. 总体架构

```
交易主机 pixiu-host（c7i.4xlarge）              ledger 小机型（常开）
┌──────────────────────────────┐  remote write  ┌─────────────────────────────────────┐
│ pixiu.service                │ ─────────────▶ │ Prometheus（已有）                  │
│   /metrics（loopback）        │                │   PnL 录制规则（已有）               │
│   journal-YYYYMMDD-*.jsonl   │                │   告警规则（从 Grafana 迁来）         │
│   binance-md / bitget-public │                └──────────────▲──────────────────────┘
│   *.pxr                      │                               │ HTTP API
│                              │   研究副本     ┌──────────────┴──────────────────────┐
│ research-upload.sh（已有）    │ ────────────▶ │ S3 研究桶（已有）                    │
│                              │                │   <host>/<day>/*.pxr.gz、journal.gz  │
│ desk-backtest.sh（新）        │ ────────────▶ │   backtests/<run-id>/（新）          │
│   pixiu 停止时才跑            │                └──────────────┬──────────────────────┘
└──────────────────────────────┘                               │ aws s3（实例角色）
                                                ┌──────────────▼──────────────────────┐
                                                │ pixiu-desk（新，单进程）              │
                                                │   ingest → SQLite → calc → axum web │
                                                │   127.0.0.1:<port>，SSM 端口转发访问 │
                                                └─────────────────────────────────────┘
```

desk 进程内部的模块：

| 模块 | 职责 |
|---|---|
| `ingest` | 定时列出 S3 新文件。按 sha256 去重，解压并解析 journal、`.pxr`、回测输出，写入 SQLite |
| `store` | SQLite 单文件（`rusqlite`，bundled），带 schema 迁移。原始事件只追加，派生表可以随时重建 |
| `calc` | FIFO 实现盈亏、markout、核对。实盘和回测走同一份代码 |
| `prom` | 查询 Prometheus HTTP API：实时曲线、录制规则、`/api/v1/alerts` |
| `web` | axum 提供 JSON API 和静态页面（页面编译进二进制） |
| 前端 | 从 Bedrock 的 `index.html` 和 `analytics.js` 改造。原生 JS 加 ECharts 5.5.0，本地托管，不走 CDN，不需要构建 |

## 3. 数据源对应

| 页面数据 | 来源 | 时效 |
|---|---|---|
| 运行状态、模式、hold、feed、持仓、在场单 | Prometheus：`pixiu_up`、`pixiu_mode`、`pixiu_hold`、`pixiu_feed_health`、`pixiu_position`、`pixiu_live_orders` 等 | 实时（秒级） |
| 实盘盈亏曲线、权益线、体系外部分 | Prometheus 录制规则 `pixiu:session_pnl:usdt`、`pixiu:equity_change:usdt`、`pixiu:outside_pnl:usdt`、`pixiu:fees:usdt` 等 | 实时 |
| 告警 | Prometheus `/api/v1/alerts`（规则迁移后） | 实时 |
| 逐笔成交、手续费、FIFO 盈亏 | journal 的 `fill`、`order_fee`、`order_registered`、`booked` 记录 | T+1（见第 8 节） |
| markout 所需的中间价 | `.pxr`：binance-md（bookTicker）和 bitget-public（ticker、books1） | T+1 |
| 回测结果 | `backtests/<run-id>/`：`summary.json`、`journal.jsonl`、`basis-*.tsv` | 回测结束上传后 |

## 4. 存储模型（SQLite）

原始层只追加，派生层可以从原始层重建（Pixiu spec 12.6 的「事件存储加投影」原则，规模缩小到个人用）。

```
ingested_files(key, sha256, kind, bytes, ingested_at, status, error)
runs(run_id, source ['live'|'backtest'], started_ns, stopped_ns, config_hash, version, mode, meta_json)
fills(run_id, ts_ns, instrument, side, price, qty, exec_id_hash, client_oid, level, scope, booked)
fees(run_id, ts_ns, instrument, coid, coin, fee)               -- 带符号，按交易所原样
mids(source ['bitget'|'binance'], instrument, ts_ns, bid, ask)  -- 降采样到 100 ms（可配置）
-- 派生层
markouts(run_id, fill_rowid, horizon_s, ref, sample_ts_ns, bps, quote)
pnl_fifo(run_id, instrument, realised, unrealised, fees_by_coin_json, mark_source)
backtest_params(run_id, params_json, model_note, fill_model, tape_time, inputs_json)
```

- **金额**：价格和数量按十进制字符串入库，计算用定点整数（照搬 Pixiu 的 mantissa 和 scale 约定），不用浮点。只有在输出给图表时才转成 f64。
- **mids 体积**：两个 venue、每个合约每 100 ms 一行，每天约 1.7 M 行，约 60 MB。所以 mids 只保留成交前后 15 分钟（markout 最长 900 s）的窗口，加上全天 1 s 的降采样曲线。

## 5. 计算

### 5.1 markout（和 `live_report.py` 第 8 节完全一致）

- horizon：0、2、10、60、300、900 秒。容差 1.5 秒。参考价：Bitget 中间价、Binance 中间价。
- `bps = sign × (mid − execPrice) / execPrice × 10000`，买入 sign = +1，卖出 sign = −1。正值表示市场往有利于这笔成交的方向走了。
- quote 金额：`sign × (mid − execPrice) × execQty`。
- 采样规则：`markout_sample(times, t, h, tol)`，取 `t + h` 之后的第一个样本，必须在容差内，否则记缺失。
- 聚合：按 horizon 和参考价分组，再按 level、方向、小时分组。

**验收**：同一份 journal 加 basis，desk 的结果要和 `live_report.py --json` 的 markout 段逐格相等（同一个 scale 下整数相等）。这是 desk 的第一条测试，以 Pixiu 仓库里的 journal fixture 作为黄金样本。

### 5.2 盈亏

- FIFO 已实现盈亏、按 mark 计价的未实现盈亏、按币种的手续费，和 `live_report.py` 第 3 节一致。
- 实盘曲线以 Prometheus 录制规则为准（权威）。desk 的 FIFO 结果和它交叉核对，差异显示在「数据核对」页，不自动修正。

### 5.3 数据核对（对应 Bedrock 的「数据核对」页）

- `fill` 和 `booked` 的配对率，journal 丢记录的计数（`pixiu_journal_money_dropped_total`），覆盖缺口（某段时间没有 journal 或 `.pxr`），journal 时钟和交易所时钟的偏差。

## 6. 回测

### 6.1 在 pixiu-host 上跑（推荐）

可以在 pixiu-host 上跑，而且比在 ledger 机器上跑更合适：

- 8 个物理核，最近 7 天的 `.pxr` 就在本地磁盘上，不用下载。`pixiu-backtest` 是 scenarios crate 里的开发用二进制，我还没确认主机上有没有，可能要在主机上编译或拷贝一次（属于主机操作）。
- 你本来就不会让回测和实盘同时跑。包装脚本会把这条规则硬性执行：`pixiu.service` 处于 active 状态时拒绝启动，回测进行中也拒绝启动实盘（写一个锁文件，把检查锁文件加进 unit 的 `ExecStartPre`。这一项要改 Pixiu 的 unit，需要单独定）。

`deploy/desk-backtest.sh`（放在本仓库）：

```
desk-backtest.sh --config <file> --day <YYYYMMDD> [--from/--to] [--fill-model trades] [--label <text>]
  1. 检查 pixiu.service 不在运行，不满足就退出
  2. 生成 run-id（UTC 时间加短哈希），建输出目录
  3. nice -n 10 跑 pixiu-backtest，只放在非隔离核上（taskset 指定 housekeeping 核）
  4. 写 desk-meta.json（label、命令行、pixiu 版本、git 提交、输入文件的 sha256）
  5. aws s3 cp --recursive <out> s3://<bucket>/backtests/<run-id>/
```

desk 的 ingest 发现 `backtests/<run-id>/` 后入库，回测结果会自动出现在网页上。

### 6.2 desk 能触发回测吗？

第一版不能。触发回测等于在交易主机上执行命令，是主机操作，每次都要在会话里批准，而且需要 SSM 权限。第一版按上面的方式手动跑（或者让我通过 aws-host-ops 跑）。以后如果需要，可以在 desk 里做一个「生成命令」按钮，只拼命令行，不执行。

## 7. 页面（从 Bedrock 改造）

| Bedrock 页面 | desk 页面 | 改动 |
|---|---|---|
| 总览（账户、原子表、KILL/RESUME） | **总览**：进程状态、每个合约一行（模式、持仓、在场单、今日成交、今日盈亏）、告警条 | 去掉 KILL/RESUME；数据从 gateway 改为 Prometheus |
| 原子页「操作」（四联图、Ladder、参数卡、操作日志） | **实盘**：24h 四联图（累计盈亏、净值、成交量、持仓），参数卡读 `pixiu_build_info` 的 config_hash 和当次配置 | 去掉写操作和 requestId 跟踪；Ladder 图（在场单阶梯）在 P3 做 |
| 原子页「分析」（瀑布、核对、回放泳道、参数时代） | **分析**：瀑布拆分、**markout**（horizon 曲线，按 level、方向、小时）、数据核对、参数时代对比（按 config_hash 切分） | 后端从 Java 分析服务改为 desk |
| 无 | **回测**：运行列表；详情页（参数、累计曲线、成交、markout、`summary.json` 原文）；勾选多次做对比；选一段实盘叠加对比 | 新增 |

主题（亮色、暗色、跟随系统）、等宽数字、`?` 术语说明这些交互全部保留。

## 8. 已知限制

1. **逐笔数据是 T+1。** `research-upload.sh` 只上传已经关闭的文件（journal 按天关闭，`.pxr` 按小时）。当天的 markout 要等第二天才有。当天的盈亏曲线和状态不受影响，它们走 Prometheus。如果要当天看 markout，可以在 P2 加一个按小时切 journal 并上传的方案（要改 Pixiu 的 hk 或者上传脚本，单独定）。
2. **研究副本从 2026-10-21 开始才有。** 更早的 run 只有 Prometheus 里的预计算曲线，没有逐笔 markout。
3. **没有 `ledger sync`（Pixiu M10.4 仍延后）。** 资金费明细、你的手工平仓不在 journal 里，只体现在权益线里。desk 把这一块显示为「体系外」，不逐条拆分。

## 9. Grafana 退役

1. 31 条告警规则改写为 Prometheus 原生 alerting rules，部署到 ledger 机器的 Prometheus。desk 读 `/api/v1/alerts` 并显示在总览顶部。
2. desk P1 上线后，对照一份清单（Grafana 两个看板的每个 panel 在 desk 里都有对应）验收。
3. 停掉并卸载 Grafana，删除 Pixiu 仓库的 `deploy/grafana/`，把 `crates/app/tests/dashboards.rs` 改为检查 Prometheus 规则文件。这是一个 Pixiu 计划任务，加一次 ledger 主机变更。

说明：目前仓库里没有配置 Grafana 的 contact point（告警推送渠道），所以现在告警只能在 Grafana 里看到。迁移后在 desk 里看，效果不变。推送通知（out-of-band notifier）在 Pixiu 那边仍然是延后状态。

## 10. 技术选型

| 项 | 选择 | 理由 |
|---|---|---|
| 语言、工具链 | Rust 1.97，edition 2024 | 和 Pixiu 一致 |
| Web | `axum` 加 `tokio` | desk 不是热路径，用 async 没问题 |
| 存储 | `rusqlite`（bundled） | 单文件，零运维，小机型友好 |
| S3 | 调用 `aws` CLI 子进程 | 省掉 aws-sdk 一大串依赖；ledger 机器上已有实例角色 |
| Prometheus | `reqwest` 查询 HTTP API | — |
| 解压、JSON | `flate2`、`serde`、`serde_json` | — |
| 前端 | 原生 JS 加 ECharts 5.5.0，用 `include_bytes!` 编译进二进制 | 沿用 Bedrock，不需要构建 |
| 部署 | systemd unit，`127.0.0.1:<port>` | 访问方式：`aws ssm start-session` 端口转发 |

## 11. 分期和验收

### P1：盈亏和 markout（最低要求）

- [ ] 仓库骨架：workspace、`scripts/check.sh`（fmt、clippy、test）、AGENTS.md
- [ ] journal 解析加 FIFO 加 markout，以 `live_report.py --json` 为黄金样本逐格比对
- [ ] `.pxr` 读取（格式见 Pixiu spec 4.6、`scripts/research/pxr.py`），产出 mids
- [ ] S3 ingest，按 sha256 幂等
- [ ] Prometheus 查询，总览页和实盘页
- [ ] 分析页：瀑布拆分、markout、数据核对
- [ ] systemd unit 加部署说明（在 ledger 机器上安装需要你批准）

**验收**：用 run 8 到 11 的研究副本，网页上的 markout 和 `live_report.py` 一致，实盘曲线和 Grafana 一致。

### P2：回测

- [ ] `desk-backtest.sh`（守卫、nice、上传）
- [ ] 回测 ingest，列表页、详情页、对比页、实盘叠加
- [ ] 告警迁移到 Prometheus，Grafana 退役清单

**验收**：在 pixiu-host 上跑一次回测，结果自动出现在 desk 上，和 `summary.json` 的数值一致。

### P3：可选

- Ladder 和在场单视图、单笔回放泳道、当天 markout（按小时上传）、受控写操作（loopback 桥接加鉴权）。

## 12. 需要你确认的问题

1. 文档和界面语言：页面用中文，代码、注释和提交信息用英文（和 Pixiu 一致）。可以吗？
2. S3 研究桶的名字和 host label：你提供，或者我从主机配置里读（读取需要你批准）。
3. markout 的 mids 降采样定为 100 ms，可以吗？（`live_report.py` 用的是 basis 采样间隔，回测默认 `--basis-interval-ms`。）
4. 「回测进行中禁止启动实盘」的锁：要加进 Pixiu unit 的 `ExecStartPre` 吗？还是只靠你自己的习惯加上脚本单向检查？
