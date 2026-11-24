use super::*;
use crate::analysis::analyse;

fn journal(lines: &[serde_json::Value]) -> Journal {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    Journal::read_all([text.as_bytes()]).expect("读取")
}

fn find<'a>(groups: &'a [Group], name: &str) -> &'a Item {
    groups
        .iter()
        .flat_map(|g| &g.items)
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("没有核对项 {name}"))
}

#[test]
fn consistent_run_passes() {
    use serde_json::json;
    let j = journal(&[
        json!({"ts_wall_ns": 0, "kind": "run_start", "mode": "live"}),
        json!({"ts_wall_ns": 1, "kind": "position_snapshot", "instrument": 0, "net_mantissa": 0, "drift": "seeded"}),
        json!({"ts_wall_ns": 2, "kind": "order_registered", "coid": 7, "client_oid": "bdp7t02020100", "instrument": 0}),
        json!({"ts_wall_ns": 3, "kind": "fill", "instrument": 0, "exec_id_hash": 1, "price_mantissa": 10_000_000_000i64,
               "qty_mantissa": 100_000_000, "side": "buy", "client_oid": "bdp7t02020100", "booked": true}),
        json!({"ts_wall_ns": 3, "kind": "booked", "coid": 7, "booking": "fill", "delta_mantissa": 100_000_000, "net_mantissa": 100_000_000}),
        json!({"ts_wall_ns": 4, "kind": "order_fee", "instrument": 0, "client_oid": "bdp7t02020100", "coin": "USDT", "fee_mantissa": 1_000_000_000i64}),
        json!({"ts_wall_ns": 5, "kind": "position_snapshot", "instrument": 0, "net_mantissa": 100_000_000, "drift": "clean"}),
        json!({"ts_wall_ns": 6, "kind": "run_stop", "reason": "signal", "exit_code": 0}),
    ]);
    let symbols = BTreeMap::from([(0, "BTCUSDT".to_owned())]);
    let p = Params::default();
    let a = analyse(&j, Some(&symbols), &BTreeMap::new(), &p).expect("分析");
    let g = check(&j, &a, &BTreeMap::new(), None, &p).expect("核对");
    for name in [
        "非法行",
        "运行边界",
        "退出",
        "有成交无 booked",
        "有 booked 无成交",
        "数量不一致的订单",
        "有成交但没有手续费的订单",
        "BTCUSDT 持仓",
        "BTCUSDT 漂移",
    ] {
        assert_eq!(
            find(&g, name).verdict,
            Verdict::Ok,
            "{name}：{:?}",
            find(&g, name)
        );
    }
    assert_eq!(find(&g, "BTCUSDT 样本").verdict, Verdict::Warn, "没有样本");
}

#[test]
fn inconsistencies_are_flagged() {
    use serde_json::json;
    let j = journal(&[
        json!({"ts_wall_ns": 0, "kind": "run_start"}),
        json!({"ts_wall_ns": 1, "kind": "position_snapshot", "instrument": 0, "net_mantissa": 0}),
        json!({"ts_wall_ns": 2, "kind": "order_registered", "coid": 7, "client_oid": "bdp7t02020100"}),
        json!({"ts_wall_ns": 2, "kind": "order_registered", "coid": 8, "client_oid": "bdp8t02020100"}),
        // 订单 7：成交 1，booked 0.5；订单 8：只有 booked；还有一笔不认识的成交。
        json!({"ts_wall_ns": 3, "kind": "fill", "instrument": 0, "exec_id_hash": 1, "price_mantissa": 1, "qty_mantissa": 100_000_000,
               "side": "buy", "client_oid": "bdp7t02020100"}),
        json!({"ts_wall_ns": 3, "kind": "booked", "coid": 7, "booking": "fill", "delta_mantissa": 50_000_000}),
        json!({"ts_wall_ns": 3, "kind": "booked", "coid": 8, "booking": "fill", "delta_mantissa": 1}),
        json!({"ts_wall_ns": 4, "kind": "fill", "instrument": 0, "exec_id_hash": 2, "price_mantissa": 1, "qty_mantissa": 1,
               "side": "sell", "client_oid": "manual", "booked": false}),
        json!({"ts_wall_ns": 5, "kind": "position_snapshot", "instrument": 0, "net_mantissa": 5, "drift": "drifted"}),
        "not json".into(),
    ]);
    let symbols = BTreeMap::from([(0, "BTCUSDT".to_owned())]);
    let p = Params::default();
    let a = analyse(&j, Some(&symbols), &BTreeMap::new(), &p).expect("分析");
    let g = check(&j, &a, &BTreeMap::new(), None, &p).expect("核对");
    for name in [
        "运行边界",
        "引擎未入账的成交",
        "有成交无 booked",
        "有 booked 无成交",
        "数量不一致的订单",
        "有成交但没有手续费的订单",
        "BTCUSDT 持仓",
        "BTCUSDT 漂移",
    ] {
        assert_eq!(find(&g, name).verdict, Verdict::Warn, "{name}");
    }
    assert_eq!(find(&g, "有成交无 booked").value, "1");
    assert_eq!(find(&g, "有 booked 无成交").value, "1");
}
