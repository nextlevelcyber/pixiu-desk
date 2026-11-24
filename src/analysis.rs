//! 一次运行（实盘或回测）的分析：成交、手续费、FIFO 盈亏、markout。
//!
//! 每条规则都与 Pixiu 的 `scripts/lib/live_report.py` 一致（第 2、3、8 节），并以它的 `--json`
//! 输出作为黄金样本逐格比对（`tests/golden.rs`）。不同之处只有一处：desk 按合约分别取样本，
//! 所以多合约的运行也能算 markout 和 mark（`live_report.py` 只接受单合约）。

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::fx::{Fx, FxError, WORK_SCALE};
use crate::journal::{Journal, int_of};
use crate::samples::{Ref, Sample};
use crate::timefmt::utc_millis;

pub const NS: i64 = 1_000_000_000;
/// markout 的 horizon（秒），与 `live_report.py` 的 `MARKOUT_HORIZONS_S` 一致。
pub const HORIZONS_S: [i64; 6] = [0, 2, 10, 60, 300, 900];
/// `order_fee` 记录未给 `fee_scale` 时的默认值（Pixiu core 的 FEE_SCALE）。
pub const FEE_SCALE_DEFAULT: i64 = 12;

/// 分析参数。
#[derive(Debug, Clone)]
pub struct Params {
    /// 数量的小数位（`acct_qty_scale`，Pixiu 默认 8）。
    pub qty_scale: u32,
    /// 价格的小数位（`price_scale`，默认 8）。
    pub price_scale: u32,
    /// markout 样本的容差（秒，十进制）。
    pub markout_tol_s: Fx,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            qty_scale: 8,
            price_scale: 8,
            markout_tol_s: Fx::new(15, 1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    fn parse(s: Option<&str>) -> Option<Side> {
        match s {
            Some("buy") => Some(Side::Buy),
            Some("sell") => Some(Side::Sell),
            _ => None,
        }
    }
}

/// 一笔成交（`fill` 记录，按 `exec_id_hash` 去重，取第一次出现）。
#[derive(Debug, Clone)]
pub struct Fill {
    pub ts: i64,
    pub exec_id: String,
    pub instrument: Option<i64>,
    pub symbol: String,
    pub side: Side,
    pub price: Fx,
    pub qty: Fx,
    pub client_oid: Option<String>,
    pub scope: Option<String>,
    pub level: Option<u8>,
}

fn symbol_of(symbols: &BTreeMap<i64, String>, instr: Option<i64>) -> String {
    match instr {
        Some(i) => symbols
            .get(&i)
            .cloned()
            .unwrap_or_else(|| format!("instrument {i}")),
        None => "instrument None".to_owned(),
    }
}

/// journal 里的成交，按时间排序。
pub fn fills(j: &Journal, symbols: &BTreeMap<i64, String>, p: &Params) -> Vec<Fill> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for r in j.of_kind("fill") {
        let (Some(h), Some(pm), Some(qm)) = (
            r.int("exec_id_hash"),
            r.int("price_mantissa"),
            r.int("qty_mantissa"),
        ) else {
            continue;
        };
        let Some(side) = Side::parse(r.str("side")) else {
            continue;
        };
        let id = h.to_string();
        if pm <= 0 || qm <= 0 || seen.contains(&id) {
            continue;
        }
        seen.insert(id.clone());
        let client_oid = r.str("client_oid").map(str::to_owned);
        out.push(Fill {
            ts: r.ts,
            exec_id: id,
            instrument: r.int("instrument"),
            symbol: symbol_of(symbols, r.int("instrument")),
            side,
            price: Fx::new(i128::from(pm), p.price_scale),
            qty: Fx::new(i128::from(qm), p.qty_scale),
            level: client_oid.as_deref().and_then(tag_level),
            client_oid,
            scope: r.str("scope").map(str::to_owned),
        });
    }
    out.sort_by_key(|f| f.ts);
    out
}

/// 一个订单的手续费（最后一次终态推送的各币种条目）。
#[derive(Debug, Clone)]
pub struct OrderFee {
    /// 采用的那次推送的记录时间（累计曲线按它计入手续费）。
    pub ts: i64,
    pub symbol: String,
    pub entries: Vec<(String, Fx)>,
}

/// `order_fee` 记录：每个订单（venue orderId，否则 clientOid）取最后一次终态推送（`update_ms` 最大，
/// 其次记录最晚）的全部条目，重复推送只算一次。
pub fn order_fees(j: &Journal, symbols: &BTreeMap<i64, String>) -> BTreeMap<String, OrderFee> {
    type Stamp = (i64, i64);
    let mut last: BTreeMap<String, (Stamp, OrderFee)> = BTreeMap::new();
    for r in j.of_kind("order_fee") {
        let (Some(fm), Some(coin)) = (r.int("fee_mantissa"), r.str("coin")) else {
            continue;
        };
        let fs = match r.v.get("fee_scale") {
            None | Some(Value::Null) => Some(FEE_SCALE_DEFAULT),
            other => int_of(other),
        };
        let Some(fs) = fs.filter(|s| (0..=18).contains(s)) else {
            continue;
        };
        let key = r
            .str("venue_order_id")
            .filter(|s| !s.is_empty())
            .or_else(|| r.str("client_oid").filter(|s| !s.is_empty()))
            .map_or_else(|| "None".to_owned(), str::to_owned);
        let stamp = (r.int("update_ms").unwrap_or(-1), r.ts);
        let scale = u32::try_from(fs).unwrap_or(12);
        let entry = (coin.to_owned(), Fx::new(i128::from(fm), scale));
        match last.get_mut(&key) {
            Some(held) if stamp == held.0 => held.1.entries.push(entry),
            Some(held) if stamp < held.0 => {}
            _ => {
                last.insert(
                    key,
                    (
                        stamp,
                        OrderFee {
                            ts: r.ts,
                            symbol: symbol_of(symbols, r.int("instrument")),
                            entries: vec![entry],
                        },
                    ),
                );
            }
        }
    }
    last.into_iter().map(|(k, (_, f))| (k, f)).collect()
}

/// 一个未平的 lot。
#[derive(Debug, Clone)]
pub struct Lot {
    pub qty: Fx,
    pub price: Fx,
    pub sign: i64,
}

/// FIFO：每笔成交先与最早的反向 lot 配对，已实现盈亏以计价币计。
pub fn fifo<'a>(fills: impl IntoIterator<Item = &'a Fill>) -> Result<(Fx, Vec<Lot>), FxError> {
    let mut lots: std::collections::VecDeque<Lot> = std::collections::VecDeque::new();
    let mut realised = Fx::ZERO;
    for f in fills {
        let sign = f.side.sign();
        let mut q = f.qty;
        while q.signum() > 0 {
            let Some(lot) = lots.front_mut() else { break };
            if lot.sign != -sign {
                break;
            }
            let m = if q < lot.qty { q } else { lot.qty };
            realised = realised.plus(m.times(f.price.minus(lot.price)?)?.mul_int(lot.sign)?)?;
            lot.qty = lot.qty.minus(m)?;
            q = q.minus(m)?;
            if lot.qty.is_zero() {
                lots.pop_front();
            }
        }
        if q.signum() > 0 {
            lots.push_back(Lot {
                qty: q,
                price: f.price,
                sign,
            });
        }
    }
    Ok((realised, lots.into_iter().collect()))
}

/// clientOid 标签里的 ladder level（与 Pixiu `crates/core` ids.rs 的解码一致）：
/// `bdp<coid>t<hex8>`，v2 做市负载（version 2、kind 2、side 1 或 2），level 是负载的最低字节。
pub fn tag_level(client_oid: &str) -> Option<u8> {
    let rest = client_oid.strip_prefix("bdp")?;
    let (digits, tag) = match rest.split_once('t') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let tag = tag?;
    if tag.is_empty() || tag.len() > 8 || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() || trimmed.len() > 20 || trimmed.parse::<u64>().is_err() {
        return None;
    }
    let payload = u32::from_str_radix(tag, 16).ok()?;
    let [version, kind, side, level] = payload.to_be_bytes();
    (version == 2 && kind == 2 && (side == 1 || side == 2)).then_some(level)
}

/// 报价时长：引擎模式为 `running` 的时间（观察模式的运行不算），单位 ns；没有则为 `None`。
pub fn quoting_span_ns(j: &Journal) -> Option<i64> {
    let (start, end) = (j.first_ts?, j.last_ts?);
    let mut total: i64 = 0;
    let mut opened: Option<i64> = None;
    let mut observe = false;
    for r in &j.records {
        if !matches!(r.kind.as_str(), "mode" | "run_start" | "run_stop") {
            continue;
        }
        if let Some(o) = opened.take() {
            total += 0.max(r.ts.min(end) - o.max(start));
        }
        if r.kind == "run_start" {
            observe = r.str("mode") == Some("observe");
        } else if r.kind == "mode" && r.str("to") == Some("running") && !observe {
            opened = Some(r.ts);
        }
    }
    if let Some(o) = opened {
        total += 0.max(end - o.max(start));
    }
    (total != 0).then_some(total)
}

/// markout 样本的下标（`times` 已排序）：h > 0 时取 `t + h` 及之后的第一个样本且不晚于容差；
/// h = 0 时取两侧容差内最近的，距离相同取较早的。
pub fn markout_sample(times: &[i64], t: i64, h_ns: i64, tol_ns: Fx) -> Option<usize> {
    let within = |d: i64| Fx::from_int(d) <= tol_ns;
    if h_ns == 0 {
        let i = times.partition_point(|&x| x < t);
        let mut best: Option<usize> = None;
        for j in [i.checked_sub(1), Some(i)].into_iter().flatten() {
            if j < times.len() && within((times[j] - t).abs()) {
                best = match best {
                    Some(b)
                        if ((times[b] - t).abs(), times[b]) <= ((times[j] - t).abs(), times[j]) =>
                    {
                        Some(b)
                    }
                    _ => Some(j),
                };
            }
        }
        return best;
    }
    let target = t + h_ns;
    let i = times.partition_point(|&x| x < target);
    (i < times.len() && within(times[i] - target)).then_some(i)
}

fn pct(part: usize, whole: usize) -> Result<String, FxError> {
    let v = Fx::from_int(i64::try_from(part).unwrap_or(i64::MAX)).mul_int(100)?;
    Ok(format!(
        "{}%",
        v.div_int(i64::try_from(whole).unwrap_or(i64::MAX), 2)?
    ))
}

/// 线性插值分位数（位置 (n - 1) × q），q = 0.5 即中位数。
fn median(sorted: &[Fx]) -> Result<Fx, FxError> {
    let n = sorted.len();
    let low = (n - 1) / 2;
    let high = (low + 1).min(n - 1);
    if (n - 1).is_multiple_of(2) {
        return Ok(sorted[low]);
    }
    let half = sorted[high]
        .minus(sorted[low])?
        .div_int(2, WORK_SCALE + 1)?;
    sorted[low].plus(half)
}

/// 一个聚合格：样本数、bps 均值和中位数、计价币合计、正值占比。
fn markout_stats(bps: &[Fx], quotes: &[Fx]) -> Result<Value, FxError> {
    let n = bps.len();
    let positive = bps.iter().filter(|v| v.signum() > 0).count();
    if n == 0 {
        return Ok(
            json!({"count": 0, "mean_bps": null, "median_bps": null, "quote_sum": null,
                         "positive": 0, "positive_share": null}),
        );
    }
    let mut sum = Fx::ZERO;
    for v in bps {
        sum = sum.plus(*v)?;
    }
    let mut qsum = Fx::ZERO;
    for v in quotes {
        qsum = qsum.plus(*v)?;
    }
    let mut sorted = bps.to_vec();
    sorted.sort();
    let nn = i64::try_from(n).unwrap_or(i64::MAX);
    Ok(json!({
        "count": n,
        "mean_bps": sum.div_int(nn, 3)?.fixed_unsigned_zero(3)?,
        "median_bps": median(&sorted)?.fixed_unsigned_zero(3)?,
        "quote_sum": qsum.fixed_unsigned_zero(6)?,
        "positive": positive,
        "positive_share": pct(positive, n)?,
    }))
}

/// 一笔成交在一个 horizon、一个参考价下的 markout。
#[derive(Debug, Clone)]
pub struct MarkoutCell {
    pub horizon_s: i64,
    pub reference: Ref,
    pub sample_ts: i64,
    pub bps: Fx,
    pub quote: Fx,
}

/// 一笔成交的全部 markout 格（缺样本的格不出现）。
pub fn markout_cells(f: &Fill, samples: &[Sample], tol_s: Fx) -> Result<Vec<MarkoutCell>, FxError> {
    let times: Vec<i64> = samples.iter().map(|s| s.ts).collect();
    markout_cells_with(f, samples, &times, tol_s.mul_int(NS)?)
}

fn markout_cells_with(
    f: &Fill,
    samples: &[Sample],
    times: &[i64],
    tol_ns: Fx,
) -> Result<Vec<MarkoutCell>, FxError> {
    let mut out = Vec::new();
    for h in HORIZONS_S {
        let Some(j) = markout_sample(times, f.ts, h * NS, tol_ns) else {
            continue;
        };
        for r in Ref::ALL {
            let mid = samples[j].mid_of(r)?;
            let mv = mid.minus(f.price)?.mul_int(f.side.sign())?;
            out.push(MarkoutCell {
                horizon_s: h,
                reference: r,
                sample_ts: samples[j].ts,
                bps: mv.mul_int(10_000)?.div(f.price, WORK_SCALE)?,
                quote: mv.times(f.qty)?,
            });
        }
    }
    Ok(out)
}

type Buckets = BTreeMap<(i64, Ref), (Vec<Fx>, Vec<Fx>)>;

fn empty_buckets() -> Buckets {
    let mut b = Buckets::new();
    for h in HORIZONS_S {
        for r in Ref::ALL {
            b.insert((h, r), (Vec::new(), Vec::new()));
        }
    }
    b
}

fn by_horizon(b: &Buckets) -> Result<Value, FxError> {
    let mut out = serde_json::Map::new();
    for h in HORIZONS_S {
        let mut per = serde_json::Map::new();
        for r in Ref::ALL {
            let (bps, quotes) = &b[&(h, r)];
            per.insert(r.key().to_owned(), markout_stats(bps, quotes)?);
        }
        out.insert(h.to_string(), Value::Object(per));
    }
    Ok(Value::Object(out))
}

/// 一个合约的 markout 报告，结构与 `live_report.py --json` 的 `markout` 一致。
pub fn markout_report(
    fills: &[&Fill],
    samples: &[Sample],
    tol_s: Fx,
    span_ns: Option<i64>,
) -> Result<Value, FxError> {
    let times: Vec<i64> = samples.iter().map(|s| s.ts).collect();
    let tol_ns = tol_s.mul_int(NS)?;
    let mut level_order: Vec<Option<u8>> = fills
        .iter()
        .filter_map(|f| f.level)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(Some)
        .collect();
    if fills.iter().any(|f| f.level.is_none()) {
        level_order.push(None);
    }
    let mut values = empty_buckets();
    let mut level_values: BTreeMap<Option<u8>, Buckets> = level_order
        .iter()
        .map(|lv| (*lv, empty_buckets()))
        .collect();
    let mut per_fill = Vec::new();
    let mut missing = 0u64;
    for f in fills {
        let cells = markout_cells_with(f, samples, &times, tol_ns)?;
        let mut hz = serde_json::Map::new();
        for h in HORIZONS_S {
            let mine: Vec<&MarkoutCell> = cells.iter().filter(|c| c.horizon_s == h).collect();
            let mut cell = serde_json::Map::new();
            cell.insert(
                "sample_ns".into(),
                mine.first().map_or(Value::Null, |c| json!(c.sample_ts)),
            );
            if mine.is_empty() {
                missing += 1;
            }
            for r in Ref::ALL {
                let c = mine.iter().find(|c| c.reference == r);
                let (b, q) = match c {
                    Some(c) => (
                        json!(c.bps.fixed_unsigned_zero(3)?),
                        json!(c.quote.fixed_unsigned_zero(6)?),
                    ),
                    None => (Value::Null, Value::Null),
                };
                cell.insert(format!("{}_bps", r.key()), b);
                cell.insert(format!("{}_quote", r.key()), q);
                if let Some(c) = c {
                    for target in [
                        values.get_mut(&(h, r)),
                        level_values
                            .get_mut(&f.level)
                            .and_then(|lv| lv.get_mut(&(h, r))),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        target.0.push(c.bps);
                        target.1.push(c.quote);
                    }
                }
            }
            hz.insert(h.to_string(), Value::Object(cell));
        }
        per_fill.push(json!({
            "ts_ns": f.ts, "exec_id": f.exec_id, "symbol": f.symbol, "side": f.side.as_str(),
            "price": f.price.to_plain(), "qty": f.qty.to_plain(), "level": f.level,
            "horizons": hz,
        }));
    }
    let span_s = match span_ns {
        Some(s) => Some(Fx::from_int(s).div_int(NS, 3)?.fixed(3)?),
        None => None,
    };
    let mut by_level = Vec::new();
    for lv in &level_order {
        let count = fills.iter().filter(|f| f.level == *lv).count();
        let fph = match span_ns {
            Some(s) => Some(
                Fx::from_int(i64::try_from(count).unwrap_or(i64::MAX))
                    .mul_int(3600 * NS)?
                    .div_int(s, 2)?
                    .fixed(2)?,
            ),
            None => None,
        };
        by_level.push(json!({
            "level": lv, "fills": count, "fills_per_hour": fph,
            "horizons": by_horizon(&level_values[lv])?,
        }));
    }
    Ok(json!({
        "horizons_s": HORIZONS_S,
        "tolerance_s": tol_s.to_plain(),
        "fills": per_fill,
        "aggregate": by_horizon(&values)?,
        "missing": missing,
        "quoting_span_s": span_s,
        "by_level": by_level,
    }))
}

/// 一个合约的盈亏，结构与 `live_report.py --json` 的 `pnl.symbols[...]` 一致。
#[derive(Debug, Clone, Serialize)]
pub struct PnlRow {
    pub realised: String,
    pub open_qty: String,
    pub open_avg_cost: String,
    pub mark: String,
    pub mark_source: String,
    pub unrealised: String,
    pub fees: String,
    pub total: String,
}

/// 各合约的 FIFO 盈亏：未平部分按 mark 计价（有该合约的样本时取窗口结束前最后一个样本的 Bitget
/// 中间价，否则取该合约最后一笔成交价）；total = 已实现 + 未实现 − 手续费（各币种直接相加，不换算）。
pub fn pnl(
    fills: &[Fill],
    fees: &BTreeMap<String, OrderFee>,
    samples: &BTreeMap<String, Vec<Sample>>,
    end: Option<i64>,
) -> Result<BTreeMap<String, PnlRow>, FxError> {
    let has_fees = !fees.is_empty();
    let mut fee_by_symbol: BTreeMap<String, Fx> = BTreeMap::new();
    for f in fees.values() {
        let e = fee_by_symbol.entry(f.symbol.clone()).or_insert(Fx::ZERO);
        for (_, v) in &f.entries {
            *e = e.plus(*v)?;
        }
    }
    let symbols: BTreeSet<&str> = fills.iter().map(|f| f.symbol.as_str()).collect();
    let mut out = BTreeMap::new();
    for symbol in symbols {
        let mine: Vec<&Fill> = fills.iter().filter(|f| f.symbol == symbol).collect();
        let (realised, lots) = fifo(mine.iter().copied())?;
        let mut open_qty = Fx::ZERO;
        let mut open_abs = Fx::ZERO;
        let mut cost_num = Fx::ZERO;
        for l in &lots {
            open_qty = open_qty.plus(l.qty.mul_int(l.sign)?)?;
            open_abs = open_abs.plus(l.qty)?;
            cost_num = cost_num.plus(l.qty.times(l.price)?)?;
        }
        let cost = if open_abs.is_zero() {
            "-".to_owned()
        } else {
            cost_num.div(open_abs, 8)?.to_plain()
        };
        let mark_sample = samples.get(symbol).and_then(|rows| {
            rows.iter()
                .rev()
                .find(|s| end.is_none_or(|e| s.ts <= e))
                .or_else(|| rows.first())
        });
        let (mark, source) = match mark_sample {
            Some(s) => (
                s.bitget_mid()?,
                format!("basis sample {}, Bitget mid", utc_millis(s.ts)),
            ),
            None => (
                mine.last().map_or(Fx::ZERO, |f| f.price),
                "last fill execPrice".to_owned(),
            ),
        };
        let mut unreal = Fx::ZERO;
        for l in &lots {
            unreal = unreal.plus(l.qty.times(mark.minus(l.price)?)?.mul_int(l.sign)?)?;
        }
        let fee = if has_fees {
            Some(fee_by_symbol.get(symbol).copied().unwrap_or(Fx::ZERO))
        } else {
            None
        };
        let total = realised.plus(unreal)?.minus(fee.unwrap_or(Fx::ZERO))?;
        out.insert(
            symbol.to_owned(),
            PnlRow {
                realised: realised.to_plain(),
                open_qty: open_qty.to_plain(),
                open_avg_cost: cost,
                mark: mark.to_plain(),
                mark_source: source,
                unrealised: unreal.to_plain(),
                fees: fee.map_or_else(|| "-".to_owned(), Fx::to_plain),
                total: total.to_plain(),
            },
        );
    }
    Ok(out)
}

/// 累计盈亏曲线上的一个点（按 Bitget 中间价逐点计价）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurvePoint {
    pub ts: i64,
    pub position: Fx,
    pub realised: Fx,
    pub unrealised: Fx,
    pub fees: Fx,
    pub total: Fx,
    pub mid: Fx,
}

/// 一个合约的累计盈亏曲线：在样本时间上每隔 `step_ns` 取一点（第一个样本起），把该时刻（含）之前的
/// 成交和手续费计入 FIFO，未平部分按该样本的 Bitget 中间价计价。最后一个样本总是输出，所以曲线终点与
/// [`pnl`] 在窗口结束时的 total 一致（mark 取同一个样本时）。
pub fn pnl_curve(
    fills: &[&Fill],
    fees: &[&OrderFee],
    samples: &[Sample],
    step_ns: i64,
) -> Result<Vec<CurvePoint>, FxError> {
    let mut fee_events: Vec<(i64, Fx)> = Vec::new();
    for f in fees {
        let mut sum = Fx::ZERO;
        for (_, v) in &f.entries {
            sum = sum.plus(*v)?;
        }
        fee_events.push((f.ts, sum));
    }
    fee_events.sort_by_key(|e| e.0);
    let mut out = Vec::new();
    let (mut fi, mut ei) = (0usize, 0usize);
    let mut applied: Vec<Fill> = Vec::new();
    let mut fee_sum = Fx::ZERO;
    let mut next_t: Option<i64> = None;
    for (k, s) in samples.iter().enumerate() {
        let last = k + 1 == samples.len();
        if !last && next_t.is_some_and(|t| s.ts < t) {
            continue;
        }
        next_t = Some(s.ts + step_ns);
        while fi < fills.len() && fills[fi].ts <= s.ts {
            applied.push(fills[fi].clone());
            fi += 1;
        }
        while ei < fee_events.len() && fee_events[ei].0 <= s.ts {
            fee_sum = fee_sum.plus(fee_events[ei].1)?;
            ei += 1;
        }
        let (realised, lots) = fifo(applied.iter())?;
        let mid = s.bitget_mid()?;
        let mut unreal = Fx::ZERO;
        let mut pos = Fx::ZERO;
        for l in &lots {
            unreal = unreal.plus(l.qty.times(mid.minus(l.price)?)?.mul_int(l.sign)?)?;
            pos = pos.plus(l.qty.mul_int(l.sign)?)?;
        }
        out.push(CurvePoint {
            ts: s.ts,
            position: pos,
            realised,
            unrealised: unreal,
            fees: fee_sum,
            total: realised.plus(unreal)?.minus(fee_sum)?,
            mid,
        });
    }
    Ok(out)
}

/// 一次运行的完整分析结果。
#[derive(Debug)]
pub struct Analysis {
    pub symbols: BTreeMap<i64, String>,
    pub fills: Vec<Fill>,
    pub fees: BTreeMap<String, OrderFee>,
    pub pnl: BTreeMap<String, PnlRow>,
    /// 合约 → markout 报告（结构同 `live_report.py`）。
    pub markout: BTreeMap<String, Value>,
    pub quoting_span_ns: Option<i64>,
}

/// 分析一次运行。`samples` 以交易对为键。
pub fn analyse(
    j: &Journal,
    given_symbols: Option<&BTreeMap<i64, String>>,
    samples: &BTreeMap<String, Vec<Sample>>,
    p: &Params,
) -> Result<Analysis, FxError> {
    let symbols = j.instrument_symbols(given_symbols);
    let fills = fills(j, &symbols, p);
    let fees = order_fees(j, &symbols);
    let pnl = pnl(&fills, &fees, samples, j.last_ts)?;
    let span = quoting_span_ns(j);
    let mut markout = BTreeMap::new();
    for (symbol, rows) in samples {
        let mine: Vec<&Fill> = fills.iter().filter(|f| &f.symbol == symbol).collect();
        if mine.is_empty() {
            continue;
        }
        markout.insert(
            symbol.clone(),
            markout_report(&mine, rows, p.markout_tol_s, span)?,
        );
    }
    Ok(Analysis {
        symbols,
        fills,
        fees,
        pnl,
        markout,
        quoting_span_ns: span,
    })
}

#[cfg(test)]
mod tests;
