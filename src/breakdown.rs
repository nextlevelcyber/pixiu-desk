//! 收益拆分（瀑布）：把一个合约的合计盈亏拆成价差收益、逆向选择、库存漂移、手续费四项。
//!
//! 参考价是 Bitget 中间价（交易的 venue）。对起点为空仓的一次运行：
//!
//! - 交易盈亏 = 已实现 + 未实现 = Σ sign × (mark − 成交价) × 数量；
//! - 价差收益 = Σ 0 秒 markout 的计价币金额 = Σ sign × (成交时中间价 − 成交价) × 数量；
//! - 逆向选择 = Σ (H 秒 markout − 0 秒 markout)，H 默认 60 秒：成交后价格反向走掉的部分；
//! - 库存漂移 = 交易盈亏 − Σ H 秒 markout：H 秒之后剩余持仓随价格的变动（残差）；
//! - 合计 = 价差收益 + 逆向选择 + 库存漂移 − 手续费，与 FIFO 的 total 精确相等。
//!
//! 缺样本的格按 0 计，差额自然落进库存漂移；缺失数单独报告。

use serde::Serialize;

use crate::analysis::{Fill, MarkoutCell, PnlRow};
use crate::fx::{Fx, FxError};
use crate::samples::Ref;

/// 逆向选择的截止 horizon（秒）。
pub const ADVERSE_HORIZON_S: i64 = 60;

/// 一个合约的拆分（金额为十进制字符串，计价币）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Breakdown {
    pub horizon_s: i64,
    pub edge: String,
    pub adverse: String,
    pub drift: String,
    /// 手续费（正值为成本，按交易所原样）；没有手续费记录时为 `None`。
    pub fees: Option<String>,
    pub trading: String,
    pub total: String,
    pub fills: usize,
    /// 缺 0 秒样本、缺 H 秒样本的成交数。
    pub missing_edge: usize,
    pub missing_horizon: usize,
}

/// 计算拆分。`cells` 是该合约全部成交的 markout 格（任意顺序，按 exec_id 对应）。
pub fn breakdown(
    pnl: &PnlRow,
    fills: &[&Fill],
    cells: &[(&str, &MarkoutCell)],
) -> Result<Breakdown, FxError> {
    let quote_at = |exec_id: &str, h: i64| {
        cells
            .iter()
            .find(|(id, c)| *id == exec_id && c.horizon_s == h && c.reference == Ref::Bitget)
            .map(|(_, c)| c.quote)
    };
    let mut edge = Fx::ZERO;
    let mut at_h = Fx::ZERO;
    let (mut missing_edge, mut missing_h) = (0, 0);
    for f in fills {
        match quote_at(&f.exec_id, 0) {
            Some(q) => edge = edge.plus(q)?,
            None => missing_edge += 1,
        }
        match quote_at(&f.exec_id, ADVERSE_HORIZON_S) {
            Some(q) => at_h = at_h.plus(q)?,
            None => missing_h += 1,
        }
    }
    let trading = Fx::parse(&pnl.realised)?.plus(Fx::parse(&pnl.unrealised)?)?;
    let fees = match pnl.fees.as_str() {
        "-" => None,
        s => Some(Fx::parse(s)?),
    };
    let adverse = at_h.minus(edge)?;
    let drift = trading.minus(at_h)?;
    let total = trading.minus(fees.unwrap_or(Fx::ZERO))?;
    Ok(Breakdown {
        horizon_s: ADVERSE_HORIZON_S,
        edge: edge.to_plain(),
        adverse: adverse.to_plain(),
        drift: drift.to_plain(),
        fees: fees.map(Fx::to_plain),
        trading: trading.to_plain(),
        total: total.to_plain(),
        fills: fills.len(),
        missing_edge,
        missing_horizon: missing_h,
    })
}

#[cfg(test)]
mod tests;
