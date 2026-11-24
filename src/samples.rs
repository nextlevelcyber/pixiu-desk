//! 双 venue 报价样本：每行是同一时刻 Bitget 和 Binance 的最优买卖价。
//!
//! 来源有两个：回测输出的 `basis-<SYMBOL>.tsv`（[`read_basis_tsv`]），以及实盘录制 `.pxr` 的联合采样
//! （`pxr` 模块）。markout 和按 mark 计价都只读这里的中间价。

use std::io::BufRead;

use crate::fx::{Fx, FxError};

/// 一个样本。中间价 = (bid + ask) / 2，精确保存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub ts: i64,
    pub bitget_bid: Fx,
    pub bitget_ask: Fx,
    pub binance_bid: Fx,
    pub binance_ask: Fx,
}

impl Sample {
    pub fn bitget_mid(&self) -> Result<Fx, FxError> {
        mid(self.bitget_bid, self.bitget_ask)
    }

    pub fn binance_mid(&self) -> Result<Fx, FxError> {
        mid(self.binance_bid, self.binance_ask)
    }

    /// markout 的参考价。
    pub fn mid_of(&self, r: Ref) -> Result<Fx, FxError> {
        match r {
            Ref::Bitget => self.bitget_mid(),
            Ref::Binance => self.binance_mid(),
        }
    }
}

/// markout 的两个参考：交易的 venue（Bitget）和 FAIR 的来源（Binance）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ref {
    Bitget,
    Binance,
}

impl Ref {
    pub const ALL: [Ref; 2] = [Ref::Bitget, Ref::Binance];

    pub fn key(self) -> &'static str {
        match self {
            Ref::Bitget => "bitget",
            Ref::Binance => "binance",
        }
    }
}

/// 两数之和除以 2，多保留一位小数，结果精确。
pub fn mid(bid: Fx, ask: Fx) -> Result<Fx, FxError> {
    let sum = bid.plus(ask)?;
    sum.div_int(2, sum.scale() + 1)
}

/// `basis-*.tsv` 的读取结果。
#[derive(Debug, Default)]
pub struct BasisRead {
    pub rows: Vec<Sample>,
    pub malformed: u64,
}

const NS: i128 = 1_000_000_000;

/// `ts` 列：数字按数量级区分秒、毫秒、微秒、纳秒，或 ISO 8601（实盘证据目录的采样 TSV 用它）；
/// 与 `live_report.py` 的 `parse_ts` 一致。
fn parse_ts(text: &str) -> Option<i64> {
    if text.contains('-') && text.contains('T') {
        return crate::timefmt::parse_iso_ns(text);
    }
    let d = Fx::parse(text).ok()?;
    let mag = d.abs();
    let scale: i128 = if mag >= Fx::new(100_000_000_000_000_000, 0) {
        1
    } else if mag >= Fx::new(100_000_000_000_000, 0) {
        1_000
    } else if mag >= Fx::new(100_000_000_000, 0) {
        1_000_000
    } else {
        NS
    };
    // int(d * scale)：向零截断。
    let x = d.times(Fx::new(scale, 0)).ok()?.trunc().ok()?;
    i64::try_from(x).ok()
}

/// 读 `basis-*.tsv`：表头 `ts bitget_bid bitget_ask binance_bid binance_ask`（制表符或空格分隔），
/// 无表头时按前五列解析；非法行或非正价格计入 malformed。结果按时间稳定排序。
pub fn read_basis_tsv<R: BufRead>(reader: R) -> std::io::Result<BasisRead> {
    const NAMES: [&str; 5] = [
        "ts",
        "bitget_bid",
        "bitget_ask",
        "binance_bid",
        "binance_ask",
    ];
    let mut out = BasisRead::default();
    let mut cols: Option<[usize; 5]> = None;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = if line.contains('\t') {
            line.split('\t').map(str::trim).collect()
        } else {
            line.split_whitespace().collect()
        };
        if cols.is_none() {
            let idx: Option<Vec<usize>> = NAMES
                .iter()
                .map(|n| fields.iter().position(|f| f == n))
                .collect();
            if let Some(idx) = idx {
                cols = Some([idx[0], idx[1], idx[2], idx[3], idx[4]]);
                continue;
            }
            cols = Some([0, 1, 2, 3, 4]);
        }
        let Some(c) = cols else { continue };
        let get = |i: usize| fields.get(c[i]).copied();
        let parsed = (|| {
            let ts = parse_ts(get(0)?)?;
            let p = |i: usize| Fx::parse(get(i)?).ok();
            Some(Sample {
                ts,
                bitget_bid: p(1)?,
                bitget_ask: p(2)?,
                binance_bid: p(3)?,
                binance_ask: p(4)?,
            })
        })();
        match parsed {
            Some(s)
                if [s.bitget_bid, s.bitget_ask, s.binance_bid, s.binance_ask]
                    .iter()
                    .all(|x| x.signum() > 0) =>
            {
                out.rows.push(s)
            }
            _ => out.malformed += 1,
        }
    }
    out.rows.sort_by_key(|s| s.ts);
    Ok(out)
}

#[cfg(test)]
mod tests;
