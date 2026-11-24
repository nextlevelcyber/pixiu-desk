//! 数据核对：一次运行的输入是否完整、彼此是否对得上。
//!
//! 每一项给出数值、判定（通过、注意、无从判断）和说明。规则来自 `live_report.py` 第 2 节（成交与 booked
//! 交叉核对）和 Pixiu spec 12.5（journal 的种类与 D18(d) 的可丢弃性），加上 desk 自己的样本覆盖检查。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::analysis::{Analysis, Fill, Params};
use crate::fx::{Fx, FxError};
use crate::journal::Journal;
use crate::samples::Sample;

/// 判定。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Ok,
    Warn,
    Na,
}

/// 一项核对。
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub name: String,
    pub value: String,
    pub verdict: Verdict,
    pub note: String,
}

/// 一组核对。
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub group: String,
    pub items: Vec<Item>,
}

fn item(name: &str, value: impl ToString, verdict: Verdict, note: impl Into<String>) -> Item {
    Item {
        name: name.to_owned(),
        value: value.to_string(),
        verdict,
        note: note.into(),
    }
}

fn ok_if(cond: bool) -> Verdict {
    if cond { Verdict::Ok } else { Verdict::Warn }
}

fn ids(list: &[String], max: usize) -> String {
    let mut s = list
        .iter()
        .take(max)
        .cloned()
        .collect::<Vec<_>>()
        .join("、");
    if list.len() > max {
        s.push_str(&format!(" 等 {} 个", list.len()));
    }
    s
}

/// 核对一次运行。`samples` 是用于 markout 的样本（以交易对为键），`sample_ms` 是其间隔。
pub fn check(
    j: &Journal,
    a: &Analysis,
    samples: &BTreeMap<String, Vec<Sample>>,
    sample_ms: Option<i64>,
    p: &Params,
) -> Result<Vec<Group>, FxError> {
    let mut groups = Vec::new();

    // ---- journal ----
    let mut g = Vec::new();
    g.push(item(
        "记录行数",
        j.lines,
        Verdict::Ok,
        format!("其中保留 {} 行用于分析", j.records.len()),
    ));
    g.push(item(
        "非法行",
        j.malformed,
        ok_if(j.malformed == 0),
        "不是 JSON 对象，或缺整数 ts_wall_ns、字符串 kind",
    ));
    let starts = j.kinds.get("run_start").copied().unwrap_or(0);
    let stops = j.kinds.get("run_stop").copied().unwrap_or(0);
    g.push(item(
        "运行边界",
        format!("run_start {starts}，run_stop {stops}"),
        ok_if(starts == 1 && stops == 1),
        "一次完整运行应各有一条；缺 run_stop 说明进程未正常退出或 journal 不完整",
    ));
    if let Some(r) = j.of_kind("run_stop").last() {
        let reason = r.str("reason").unwrap_or("—");
        let code = r.int("exit_code");
        g.push(item(
            "退出",
            format!(
                "{reason}，退出码 {}",
                code.map_or("—".to_owned(), |c| c.to_string())
            ),
            ok_if(code == Some(0)),
            "",
        ));
    }
    let mut kinds: Vec<String> = j.kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
    kinds.sort();
    g.push(item("记录种类", kinds.len(), Verdict::Ok, kinds.join("，")));
    groups.push(Group {
        group: "journal".into(),
        items: g,
    });

    // ---- 成交与 booked ----
    let mut g = Vec::new();
    let raw_fills = j.kinds.get("fill").copied().unwrap_or(0);
    g.push(item(
        "成交记录",
        format!("{raw_fills} 条，去重后 {} 笔", a.fills.len()),
        Verdict::Ok,
        "按 exec_id_hash 去重，取第一次出现",
    ));
    let unbooked: Vec<String> = j
        .of_kind("fill")
        .filter(|r| r.v.get("booked").and_then(serde_json::Value::as_bool) == Some(false))
        .filter_map(|r| r.int("exec_id_hash").map(|h| h.to_string()))
        .collect();
    g.push(item(
        "引擎未入账的成交",
        unbooked.len(),
        ok_if(unbooked.is_empty()),
        if unbooked.is_empty() {
            "fill 记录的 booked 全为 true".to_owned()
        } else {
            format!("booked=false（订单未知）：{}", ids(&unbooked, 5))
        },
    ));
    // coid → client_oid（order_registered）。
    let reg: HashMap<i64, String> = j
        .of_kind("order_registered")
        .filter_map(|r| Some((r.int("coid")?, r.str("client_oid")?.to_owned())))
        .collect();
    let mut booked_qty: BTreeMap<String, Fx> = BTreeMap::new();
    let mut booked_fill_oids: Vec<(i64, Option<String>)> = Vec::new();
    for r in j.of_kind("booked") {
        let (Some(dm), Some(coid)) = (r.int("delta_mantissa"), r.int("coid")) else {
            continue;
        };
        let oid = reg.get(&coid).cloned();
        let delta = Fx::new(i128::from(dm), p.qty_scale);
        let signed = if r.str("booking") == Some("correct_down") {
            delta.negated()
        } else {
            delta
        };
        if r.str("booking") == Some("fill") {
            booked_fill_oids.push((coid, oid.clone()));
        }
        if let Some(o) = oid {
            let e = booked_qty.entry(o).or_insert(Fx::ZERO);
            *e = e.plus(signed)?;
        }
    }
    let mut fill_qty: BTreeMap<String, Fx> = BTreeMap::new();
    for f in &a.fills {
        if let Some(o) = &f.client_oid {
            let e = fill_qty.entry(o.clone()).or_insert(Fx::ZERO);
            *e = e.plus(f.qty)?;
        }
    }
    let no_booked: Vec<String> = a
        .fills
        .iter()
        .filter(|f| {
            f.client_oid
                .as_ref()
                .is_none_or(|o| !booked_qty.contains_key(o))
        })
        .map(|f| f.exec_id.clone())
        .collect();
    let no_fill: Vec<String> = booked_fill_oids
        .iter()
        .filter(|(_, o)| o.as_ref().is_none_or(|o| !fill_qty.contains_key(o)))
        .map(|(c, _)| c.to_string())
        .collect();
    let mismatch: Vec<String> = booked_qty
        .iter()
        .filter_map(|(o, bq)| {
            let fq = fill_qty.get(o)?;
            (bq != fq).then(|| format!("{o}（booked {}，成交 {}）", bq.to_plain(), fq.to_plain()))
        })
        .collect();
    g.push(item(
        "有成交无 booked",
        no_booked.len(),
        ok_if(no_booked.is_empty()),
        if no_booked.is_empty() {
            String::new()
        } else {
            format!("exec {}", ids(&no_booked, 5))
        },
    ));
    g.push(item(
        "有 booked 无成交",
        no_fill.len(),
        ok_if(no_fill.is_empty()),
        if no_fill.is_empty() {
            String::new()
        } else {
            format!("coid {}", ids(&no_fill, 5))
        },
    ));
    g.push(item(
        "数量不一致的订单",
        mismatch.len(),
        ok_if(mismatch.is_empty()),
        ids(&mismatch, 3),
    ));
    groups.push(Group {
        group: "成交与 booked".into(),
        items: g,
    });

    // ---- 手续费 ----
    let mut g = Vec::new();
    let filled_orders: BTreeSet<&str> = a
        .fills
        .iter()
        .filter_map(|f| f.client_oid.as_deref())
        .collect();
    let fee_records = j.kinds.get("order_fee").copied().unwrap_or(0);
    g.push(item(
        "手续费记录",
        format!("{fee_records} 条，{} 个订单", a.fees.len()),
        if a.fees.is_empty() && !a.fills.is_empty() {
            Verdict::Warn
        } else {
            Verdict::Ok
        },
        "每个订单取最后一次终态推送",
    ));
    let fee_oids: BTreeSet<String> = j
        .of_kind("order_fee")
        .filter_map(|r| r.str("client_oid").map(str::to_owned))
        .collect();
    let without_fee: Vec<String> = filled_orders
        .iter()
        .filter(|o| !fee_oids.contains(**o))
        .map(|o| (*o).to_owned())
        .collect();
    g.push(item(
        "有成交但没有手续费的订单",
        without_fee.len(),
        ok_if(without_fee.is_empty()),
        if without_fee.is_empty() {
            "订单终态推送里都有手续费".to_owned()
        } else {
            format!(
                "运行结束时可能仍未终结，或推送丢失：{}",
                ids(&without_fee, 3)
            )
        },
    ));
    let mut coins: BTreeSet<&str> = BTreeSet::new();
    for f in a.fees.values() {
        for (c, _) in &f.entries {
            coins.insert(c);
        }
    }
    g.push(item(
        "手续费币种",
        coins.iter().copied().collect::<Vec<_>>().join("、"),
        ok_if(coins.iter().all(|c| *c == "USDT")),
        "非 USDT 的手续费不换算，直接与 USDT 相加",
    ));
    groups.push(Group {
        group: "手续费".into(),
        items: g,
    });

    // ---- 持仓 ----
    let mut g = Vec::new();
    for (instr, symbol) in &a.symbols {
        let snaps: Vec<i64> = j
            .of_kind("position_snapshot")
            .filter(|r| r.int("instrument") == Some(*instr))
            .filter_map(|r| r.int("net_mantissa"))
            .collect();
        let mine: Vec<&Fill> = a.fills.iter().filter(|f| &f.symbol == symbol).collect();
        let (Some(first), Some(last)) = (snaps.first(), snaps.last()) else {
            g.push(item(
                &format!("{symbol} 持仓"),
                "—",
                Verdict::Na,
                "journal 里没有 position_snapshot",
            ));
            continue;
        };
        let mut net = Fx::new(i128::from(*first), p.qty_scale);
        for f in &mine {
            net = net.plus(f.qty.mul_int(f.side.sign())?)?;
        }
        let end = Fx::new(i128::from(*last), p.qty_scale);
        let start = Fx::new(i128::from(*first), p.qty_scale);
        g.push(item(
            &format!("{symbol} 持仓"),
            format!(
                "起始 {}，起始 + 成交 {}，最后快照 {}",
                start.to_plain(),
                net.to_plain(),
                end.to_plain()
            ),
            ok_if(net == end),
            if start.is_zero() {
                "最后一次 venue 快照应等于起始持仓加全部成交".to_owned()
            } else {
                "起始持仓不为 0：FIFO 从空仓起算，带入的持仓不计入盈亏".to_owned()
            },
        ));
        let drifted = j
            .of_kind("position_snapshot")
            .filter(|r| r.int("instrument") == Some(*instr) && r.str("drift") == Some("drifted"))
            .count();
        g.push(item(
            &format!("{symbol} 漂移"),
            format!("{drifted} 次 drifted，共 {} 次快照", snaps.len()),
            ok_if(drifted == 0),
            "引擎对 venue 快照的判定",
        ));
    }
    groups.push(Group {
        group: "持仓".into(),
        items: g,
    });

    // ---- 样本 ----
    let mut g = Vec::new();
    let tol_ns = p.markout_tol_s.mul_int(crate::analysis::NS)?;
    for symbol in a.pnl.keys() {
        let Some(rows) = samples.get(symbol) else {
            g.push(item(
                &format!("{symbol} 样本"),
                "无",
                Verdict::Warn,
                "运行目录里没有 basis TSV 或 .pxr 录制，算不了 markout",
            ));
            continue;
        };
        let max_gap = rows
            .windows(2)
            .map(|w| w[1].ts - w[0].ts)
            .max()
            .unwrap_or(0);
        // 两端各留两倍容差的余量（回测的第一个样本在起点之后一个间隔）。
        let slack = i64::try_from(tol_ns.trunc()?).unwrap_or(i64::MAX / 4) * 2;
        let start = j.of_kind("run_start").next().map(|r| r.ts).or(j.first_ts);
        let stop = j.of_kind("run_stop").last().map(|r| r.ts).or(j.last_ts);
        let head_ok = start.is_none_or(|t| rows.first().is_some_and(|s| s.ts <= t + slack));
        let tail_ok = stop.is_none_or(|t| rows.last().is_some_and(|s| s.ts >= t - slack));
        let full = head_ok && tail_ok;
        g.push(item(
            &format!("{symbol} 样本"),
            format!(
                "{} 个，间隔 {}",
                rows.len(),
                sample_ms.map_or("—".to_owned(), |m| format!("{m} ms"))
            ),
            ok_if(full),
            if full {
                "覆盖整次运行".to_owned()
            } else {
                "没有覆盖整次运行（开头或结尾缺样本）".to_owned()
            },
        ));
        let gap_ok = Fx::from_int(max_gap) <= tol_ns;
        g.push(item(
            &format!("{symbol} 最大样本间隔"),
            format!("{:.3} 秒", max_gap as f64 / 1e9),
            ok_if(gap_ok),
            "超过 markout 容差的间隔会让附近的成交缺格",
        ));
        let missing = a
            .markout
            .get(symbol)
            .and_then(|m| m.get("missing"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        g.push(item(
            &format!("{symbol} markout 缺格"),
            missing,
            ok_if(missing == 0),
            "（成交, horizon）对里容差内没有样本的个数；运行末尾的成交缺长 horizon 属正常",
        ));
    }
    groups.push(Group {
        group: "样本".into(),
        items: g,
    });
    Ok(groups)
}

#[cfg(test)]
mod tests;
