//! Pixiu Desk：Pixiu 的个人只读后台。
//!
//! 模块按数据流排列：读入（`journal`、`samples`、`pxr`）→ 计算（`analysis`）→ 存储（`store`）
//! → 导入（`ingest`）→ 对外（`prom`、`web`）。设计见 `docs/design.md`。

pub mod analysis;
pub mod fx;
pub mod journal;
pub mod report;
pub mod samples;
pub mod timefmt;
