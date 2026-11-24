use super::*;
use crate::analysis::Side;

fn fill(id: &str, side: Side, price: &str) -> Fill {
    Fill {
        ts: 0,
        exec_id: id.into(),
        instrument: Some(0),
        symbol: "BTCUSDT".into(),
        side,
        price: Fx::parse(price).expect("价"),
        qty: Fx::parse("1").expect("量"),
        client_oid: None,
        scope: None,
        level: None,
    }
}

fn cell(h: i64, r: Ref, quote: &str) -> MarkoutCell {
    MarkoutCell {
        horizon_s: h,
        reference: r,
        sample_ts: 0,
        bps: Fx::ZERO,
        quote: Fx::parse(quote).expect("额"),
    }
}

fn row(realised: &str, unrealised: &str, fees: &str, total: &str) -> PnlRow {
    PnlRow {
        realised: realised.into(),
        open_qty: "0".into(),
        open_avg_cost: "-".into(),
        mark: "0".into(),
        mark_source: String::new(),
        unrealised: unrealised.into(),
        fees: fees.into(),
        total: total.into(),
    }
}

#[test]
fn parts_add_up_to_total() {
    // 买在 99（中间价 100，边 +1），60 秒后中间价 98（−1）；卖在 101（边 +1），60 秒后 101（0）。
    let (a, b) = (fill("a", Side::Buy, "99"), fill("b", Side::Sell, "101"));
    let cells = [
        cell(0, Ref::Bitget, "1"),
        cell(60, Ref::Bitget, "-1"),
        cell(0, Ref::Binance, "9"), // Binance 参考不参与
        cell(0, Ref::Bitget, "1"),
        cell(60, Ref::Bitget, "0"),
    ];
    let pairs: Vec<(&str, &MarkoutCell)> = vec![
        ("a", &cells[0]),
        ("a", &cells[1]),
        ("a", &cells[2]),
        ("b", &cells[3]),
        ("b", &cells[4]),
    ];
    let b = breakdown(&row("2", "0", "0.3", "1.7"), &[&a, &b], &pairs).expect("拆分");
    assert_eq!(b.edge, "2");
    assert_eq!(b.adverse, "-3");
    assert_eq!(b.drift, "3", "交易盈亏 2 − 60 秒合计 −1");
    assert_eq!(b.fees.as_deref(), Some("0.3"));
    assert_eq!(b.total, "1.7");
    let sum = Fx::parse(&b.edge)
        .and_then(|x| x.plus(Fx::parse(&b.adverse)?))
        .and_then(|x| x.plus(Fx::parse(&b.drift)?))
        .and_then(|x| x.minus(Fx::parse("0.3")?))
        .expect("合计");
    assert_eq!(sum.to_plain(), b.total);
    assert_eq!((b.missing_edge, b.missing_horizon), (0, 0));
}

#[test]
fn missing_cells_fall_into_drift() {
    let a = fill("a", Side::Buy, "99");
    let b = breakdown(&row("0", "5", "-", "5"), &[&a], &[]).expect("拆分");
    assert_eq!(
        (b.edge.as_str(), b.adverse.as_str(), b.drift.as_str()),
        ("0", "0", "5")
    );
    assert_eq!(b.fees, None);
    assert_eq!((b.missing_edge, b.missing_horizon), (1, 1));
}
