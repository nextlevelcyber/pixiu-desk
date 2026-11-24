# pixiu-desk 的开发规则

Pixiu Desk 是 Pixiu 的个人只读后台：实盘状态、盈亏与 markout 分析、回测结果。单个 Rust 进程，
部署在 ledger 机器上。设计见 `docs/design.md`；它的来由是 Pixiu 仓库的
`docs/plans/handoff/2026-11-24-OWNER-desk-console.md`。

## 每次开始先读

1. `docs/design.md`。
2. 涉及 journal、`.pxr`、`summary.json` 或 Prometheus 指标格式时，读 Pixiu 仓库对应的 spec 章节
   （12.2、12.5、4.6）。这些格式是 desk 的输入接口，以 Pixiu 为准。

## 规则

- **只读。** desk 不下单、不撤单、不改 Pixiu 的任何状态，也不给 Pixiu 加接口。
- **金额不用浮点。** 计算一律用 `fx::Fx`（定点十进制），只有交给图表时才转 `f64`。
- **口径与 Pixiu 一致。** FIFO、手续费、markout 的规则跟 `scripts/lib/live_report.py` 走；任何改动都要让
  `tests/golden.rs` 继续通过。黄金样本只能用 `live_report.py` 重新生成，不能手改。
- **测试先行。** 单元测试放在模块旁的 `tests.rs`（模块末尾 `#[cfg(test)] mod tests;`），集成测试放在 `tests/`。
  每个夹具在 `tests/fixtures/README.md` 里写明来源和读取它的测试。
- **语言。** 文档、注释、提交信息、界面文字用中文；标识符用英文。
- **不入库的东西。** 桶名、host label、实例 ID、IP、任何凭证和实盘配置都不进仓库，只出现在部署机器的
  配置文件里（`config/example.toml` 只放占位符）。
- **主机操作。** 在任何 AWS 主机上安装、启动、停止、改配置，都要所有者在当次会话里批准。

## 命令

| 用途 | 命令 |
|---|---|
| 检查门（推送前必跑） | `scripts/check.sh` |
| 测试 | `cargo test --locked` |
| 黄金样本 | `cargo test --locked --test golden` |
| 单次运行报告 | `cargo run -- report --journal <f> --basis <SYMBOL>=<tsv> --instruments <SYM0,...>` |

工具链：Rust 1.97（与 Pixiu 一致）。不要自行安装或更换工具链。
