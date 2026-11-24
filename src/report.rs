//! 命令行报告：把一次运行的分析结果输出成 JSON，结构与 `live_report.py --json` 的
//! `pnl.symbols` 和 `markout` 一致，用于黄金样本比对和手工核查。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use serde_json::{Value, json};

use crate::analysis::{self, Params};
use crate::journal::Journal;
use crate::samples::{self, Sample};

/// 打开文件；`.gz` 结尾的透明解压。
pub fn open(path: &Path) -> Result<Box<dyn std::io::BufRead>> {
    let f = File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
    let r: Box<dyn Read> = if path.extension().is_some_and(|e| e == "gz") {
        Box::new(GzDecoder::new(f))
    } else {
        Box::new(f)
    };
    Ok(Box::new(BufReader::with_capacity(1 << 20, r)))
}

/// 报告的输入。
#[derive(Debug, Default)]
pub struct Inputs<'a> {
    pub journals: Vec<&'a Path>,
    /// 交易对 → basis TSV。
    pub basis: Vec<(String, &'a Path)>,
    /// 合约序号 → 交易对；为空时从 journal 推导。
    pub instruments: Option<BTreeMap<i64, String>>,
}

pub fn build(inputs: &Inputs<'_>, params: &Params) -> Result<Value> {
    let readers = inputs
        .journals
        .iter()
        .map(|p| open(p))
        .collect::<Result<Vec<_>>>()?;
    let j = Journal::read_all(readers).context("读取 journal")?;
    let mut rows: BTreeMap<String, Vec<Sample>> = BTreeMap::new();
    for (symbol, path) in &inputs.basis {
        let r = samples::read_basis_tsv(open(path)?).context("读取 basis TSV")?;
        rows.insert(symbol.clone(), r.rows);
    }
    let a = analysis::analyse(&j, inputs.instruments.as_ref(), &rows, params)?;
    Ok(json!({
        "journal": {"lines": j.lines, "malformed": j.malformed, "first_ts": j.first_ts,
                    "last_ts": j.last_ts, "kinds": j.kinds},
        "symbols": a.symbols,
        "fills": a.fills.len(),
        "pnl": a.pnl,
        "markout": a.markout,
    }))
}
