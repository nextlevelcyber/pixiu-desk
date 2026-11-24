//! 导入：把一个"运行目录"读进存储。
//!
//! 一个运行目录装着一次运行的全部输入，文件按名字识别：
//!
//! | 文件 | 必需 | 用途 |
//! |---|---|---|
//! | `journal*.jsonl[.gz]` | 是 | 成交、手续费、模式、运行边界（多个文件按名字排序后读入） |
//! | `basis-<SYMBOL>.tsv` | 二选一 | 回测的联合样本（1 秒） |
//! | `binance-md-*.pxr[.gz]` 与 `bitget-public-*.pxr[.gz]` | 二选一 | 实盘录制，desk 自己按 `sample_ms` 联合采样 |
//! | `summary.json` | 否 | 回测的 summary：合约序号到交易对的映射、参数 |
//! | `desk-meta.json` | 否 | `label`、`source`（`live` 或 `backtest`）、`instruments`（交易对列表，按序号） |
//!
//! 同一目录内容不变时（sha256 指纹相同）跳过；变了就整次替换。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::analysis::{self, MarkoutCell, Params};
use crate::journal::Journal;
use crate::pxr::{QuoteStream, Sampler, Venue};
use crate::report::open;
use crate::samples::{self, Sample};
use crate::store::{RunData, Store};

/// 导入选项。
#[derive(Debug, Clone)]
pub struct Options {
    pub params: Params,
    /// 从 `.pxr` 采样时的间隔（毫秒）。
    pub sample_ms: i64,
    /// 累计盈亏曲线的步长（毫秒）。
    pub curve_ms: i64,
    /// 指纹相同也重新导入。
    pub force: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            params: Params::default(),
            sample_ms: 100,
            curve_ms: 10_000,
            force: false,
        }
    }
}

/// 导入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Imported { run_id: String, fills: usize },
    Unchanged { run_id: String },
}

#[derive(Debug, Default)]
struct Files {
    journals: Vec<PathBuf>,
    basis: Vec<(String, PathBuf)>,
    binance: Vec<PathBuf>,
    bitget: Vec<PathBuf>,
    summary: Option<PathBuf>,
    meta: Option<PathBuf>,
}

fn classify(dir: &Path) -> Result<Files> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("读不了目录 {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    let mut f = Files::default();
    for p in names {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let base = name.strip_suffix(".gz").unwrap_or(name);
        if base.starts_with("journal") && base.ends_with(".jsonl") {
            f.journals.push(p.clone());
        } else if let Some(sym) = base
            .strip_prefix("basis-")
            .and_then(|s| s.strip_suffix(".tsv"))
        {
            f.basis.push((sym.to_owned(), p.clone()));
        } else if base.starts_with("binance-md-") && base.ends_with(".pxr") {
            f.binance.push(p.clone());
        } else if base.starts_with("bitget-public-") && base.ends_with(".pxr") {
            f.bitget.push(p.clone());
        } else if name == "summary.json" {
            f.summary = Some(p.clone());
        } else if name == "desk-meta.json" {
            f.meta = Some(p.clone());
        }
    }
    Ok(f)
}

/// 全部输入文件（名字加内容）的 sha256。
fn fingerprint(files: &Files) -> Result<String> {
    let mut all: Vec<&PathBuf> = files.journals.iter().collect();
    all.extend(files.basis.iter().map(|(_, p)| p));
    all.extend(files.binance.iter());
    all.extend(files.bitget.iter());
    all.extend(files.summary.iter());
    all.extend(files.meta.iter());
    all.sort();
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    for p in all {
        h.update(p.file_name().map(|n| n.as_encoded_bytes()).unwrap_or(b""));
        h.update([0u8]);
        let mut f = std::fs::File::open(p).with_context(|| format!("打不开 {}", p.display()))?;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        h.update([0u8]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn read_json(path: &Option<PathBuf>) -> Result<Option<(String, Value)>> {
    let Some(p) = path else { return Ok(None) };
    let text = std::fs::read_to_string(p).with_context(|| format!("读不了 {}", p.display()))?;
    let v: Value =
        serde_json::from_str(text.trim()).with_context(|| format!("{} 不是 JSON", p.display()))?;
    Ok(Some((text, v)))
}

/// 合约序号到交易对：`desk-meta.json` 的 `instruments`，否则 summary.json 的 `instruments`
/// （`{"BTCUSDT": {"instrument": 0, ...}}`），否则交给 journal 从 inbound 推导。
fn given_symbols(meta: Option<&Value>, summary: Option<&Value>) -> Option<BTreeMap<i64, String>> {
    if let Some(list) = meta
        .and_then(|m| m.get("instruments"))
        .and_then(Value::as_array)
    {
        let m: BTreeMap<i64, String> = list
            .iter()
            .enumerate()
            .filter_map(|(i, v)| Some((i64::try_from(i).ok()?, v.as_str()?.to_owned())))
            .collect();
        return Some(m);
    }
    let obj = summary?.get("instruments")?.as_object()?;
    let m: BTreeMap<i64, String> = obj
        .iter()
        .filter_map(|(sym, v)| Some((v.get("instrument")?.as_i64()?, sym.clone())))
        .collect();
    (!m.is_empty()).then_some(m)
}

/// 导入一个运行目录。`run_id` 为空时用目录名。
pub fn import_dir(
    store: &mut Store,
    dir: &Path,
    run_id: Option<&str>,
    opt: &Options,
    now_ns: i64,
) -> Result<Outcome> {
    let files = classify(dir)?;
    if files.journals.is_empty() {
        bail!("{} 里没有 journal*.jsonl", dir.display());
    }
    let run_id = match run_id {
        Some(r) => r.to_owned(),
        None => dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .context("目录名不是 UTF-8")?,
    };
    let fp = fingerprint(&files)?;
    if !opt.force && store.fingerprint(&run_id)?.as_deref() == Some(fp.as_str()) {
        return Ok(Outcome::Unchanged { run_id });
    }
    let summary = read_json(&files.summary)?;
    let meta = read_json(&files.meta)?;
    let readers = files
        .journals
        .iter()
        .map(|p| open(p))
        .collect::<Result<Vec<_>>>()?;
    let j = Journal::read_all(readers).context("读取 journal")?;
    let given = given_symbols(meta.as_ref().map(|m| &m.1), summary.as_ref().map(|s| &s.1));
    let symbols = j.instrument_symbols(given.as_ref());

    // 样本：优先 basis TSV（回测），否则从 .pxr 采样（实盘）。
    let mut samples: BTreeMap<String, Vec<Sample>> = BTreeMap::new();
    let mut sample_ms = None;
    if !files.basis.is_empty() {
        for (sym, p) in &files.basis {
            let r = samples::read_basis_tsv(open(p)?)
                .with_context(|| format!("读取 {}", p.display()))?;
            samples.insert(sym.clone(), r.rows);
        }
        sample_ms = summary
            .as_ref()
            .and_then(|s| s.1.pointer("/params/basis_interval_ms"))
            .and_then(Value::as_i64)
            .or(Some(1000));
    } else if !files.binance.is_empty() && !files.bitget.is_empty() {
        let wanted: BTreeSet<String> = symbols.values().cloned().collect();
        let opener = |v: &[PathBuf]| -> Result<Vec<Box<dyn std::io::BufRead>>> {
            v.iter().map(|p| open(p)).collect()
        };
        let sb = QuoteStream::new(Venue::Binance, opener(&files.binance)?, &wanted)?;
        let sg = QuoteStream::new(Venue::Bitget, opener(&files.bitget)?, &wanted)?;
        Sampler::new(opt.sample_ms * 1_000_000).run(sb, sg, |sym, s| {
            samples.entry(sym.to_owned()).or_default().push(s);
        })?;
        sample_ms = Some(opt.sample_ms);
    }

    let a = analysis::analyse(&j, Some(&symbols), &samples, &opt.params)?;
    let mut markouts: Vec<(String, MarkoutCell)> = Vec::new();
    let mut curves = Vec::new();
    for (sym, rows) in &samples {
        let mine: Vec<&analysis::Fill> = a.fills.iter().filter(|f| &f.symbol == sym).collect();
        for f in &mine {
            for c in analysis::markout_cells(f, rows, opt.params.markout_tol_s)? {
                markouts.push((f.exec_id.clone(), c));
            }
        }
        if mine.is_empty() {
            continue;
        }
        let fees: Vec<&analysis::OrderFee> = a.fees.values().filter(|f| &f.symbol == sym).collect();
        curves.push((
            sym.clone(),
            analysis::pnl_curve(&mine, &fees, rows, opt.curve_ms * 1_000_000)?,
        ));
    }
    // 存储里的 markout 报告去掉逐笔部分（逐笔在 markouts 表里）。
    let mut markout_json = serde_json::Map::new();
    for (sym, rep) in &a.markout {
        let mut r = rep.clone();
        if let Some(o) = r.as_object_mut() {
            o.remove("fills");
        }
        markout_json.insert(sym.clone(), r);
    }
    let run_start = j.of_kind("run_start").next();
    let run_stop = j.of_kind("run_stop").last();
    let meta_v = meta.as_ref().map(|m| &m.1);
    let source = meta_v
        .and_then(|m| m.get("source"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if run_start.and_then(|r| r.str("mode")) == Some("backtest") {
                "backtest".to_owned()
            } else {
                "live".to_owned()
            }
        });
    let label = meta_v
        .and_then(|m| m.get("label"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let data = RunData {
        run_id: &run_id,
        source: &source,
        label: label.as_deref(),
        started_ns: run_start.map(|r| r.ts).or(j.first_ts),
        stopped_ns: run_stop.map(|r| r.ts).or(j.last_ts),
        config_hash: run_start.and_then(|r| r.str("config_hash")),
        version: run_start.and_then(|r| r.str("version")),
        mode: run_start.and_then(|r| r.str("mode")),
        fingerprint: &fp,
        symbols: json!(a.symbols),
        pnl: serde_json::to_value(&a.pnl)?,
        markout: Value::Object(markout_json),
        summary: summary.as_ref().map(|s| s.0.trim()),
        meta: meta.as_ref().map(|m| m.0.trim()),
        sample_ms,
        fills: &a.fills,
        markouts: &markouts,
        curves: &curves,
    };
    store.replace_run(&data, now_ns)?;
    Ok(Outcome::Imported {
        run_id,
        fills: a.fills.len(),
    })
}

/// 当前墙钟时间（ns），仅用于记录导入时间。
pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests;
