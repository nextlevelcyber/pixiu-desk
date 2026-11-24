use super::*;

fn fill(ts: i64, side: Side, price: &str, qty: &str) -> Fill {
    Fill {
        ts,
        exec_id: ts.to_string(),
        instrument: Some(0),
        symbol: "BTCUSDT".into(),
        side,
        price: Fx::parse(price).expect("价格"),
        qty: Fx::parse(qty).expect("数量"),
        client_oid: None,
        scope: None,
        level: None,
    }
}

#[test]
fn fifo_matches_oldest_lots_first() {
    let fs = [
        fill(1, Side::Buy, "100", "1"),
        fill(2, Side::Buy, "110", "1"),
        fill(3, Side::Sell, "120", "1.5"),
    ];
    let (realised, lots) = fifo(fs.iter()).expect("fifo");
    // 1 × (120 − 100) + 0.5 × (120 − 110) = 25
    assert_eq!(realised.to_plain(), "25");
    assert_eq!(lots.len(), 1);
    assert_eq!(lots[0].qty.to_plain(), "0.5");
    assert_eq!(lots[0].price.to_plain(), "110");
}

#[test]
fn fifo_flips_to_short() {
    let fs = [
        fill(1, Side::Buy, "100", "1"),
        fill(2, Side::Sell, "90", "3"),
    ];
    let (realised, lots) = fifo(fs.iter()).expect("fifo");
    assert_eq!(realised.to_plain(), "-10");
    assert_eq!(lots[0].sign, -1);
    assert_eq!(lots[0].qty.to_plain(), "2");
}

#[test]
fn tag_level_decodes_v2_market_making_payload() {
    assert_eq!(tag_level("bdp1793149200007890907t02020100"), Some(0));
    assert_eq!(tag_level("bdp1793149200007890907t02020205"), Some(5));
    assert_eq!(tag_level("bdp17t01020100"), None, "version 1");
    assert_eq!(tag_level("bdp17t02030100"), None, "side 3");
    assert_eq!(tag_level("bdp17"), None, "无标签");
    assert_eq!(tag_level("bdp0t02020100"), None, "coid 为 0");
    assert_eq!(tag_level("manual-123"), None);
}

#[test]
fn markout_sample_rules() {
    let times = [10 * NS, 12 * NS, 20 * NS];
    let tol = Fx::parse("1.5").expect("容差").mul_int(NS).expect("ns");
    // h = 0：两侧最近，距离相同取较早的。
    assert_eq!(markout_sample(&times, 11 * NS, 0, tol), Some(0));
    assert_eq!(markout_sample(&times, 11 * NS + 1, 0, tol), Some(1));
    assert_eq!(markout_sample(&times, 15 * NS, 0, tol), None);
    // h > 0：t + h 之后第一个，且不晚于容差。
    assert_eq!(markout_sample(&times, 8 * NS, 2 * NS, tol), Some(0));
    assert_eq!(markout_sample(&times, 8 * NS, 3 * NS, tol), Some(1));
    assert_eq!(
        markout_sample(&times, 13 * NS, 2 * NS, tol),
        None,
        "20 s 超出容差"
    );
}

#[test]
fn markout_sign_and_units() {
    let s = Sample {
        ts: 2 * NS,
        bitget_bid: Fx::parse("101").expect("价"),
        bitget_ask: Fx::parse("101").expect("价"),
        binance_bid: Fx::parse("99").expect("价"),
        binance_ask: Fx::parse("99").expect("价"),
    };
    let f = fill(0, Side::Buy, "100", "2");
    let cells = markout_cells(
        &f,
        std::slice::from_ref(&s),
        Fx::parse("1.5").expect("容差"),
    )
    .expect("markout");
    let at2: Vec<_> = cells.iter().filter(|c| c.horizon_s == 2).collect();
    assert_eq!(at2.len(), 2);
    let bg = at2
        .iter()
        .find(|c| c.reference == Ref::Bitget)
        .expect("bitget");
    assert_eq!(bg.bps.to_plain(), "100", "买入后上涨 1% 为 +100 bps");
    assert_eq!(bg.quote.to_plain(), "2");
    let bn = at2
        .iter()
        .find(|c| c.reference == Ref::Binance)
        .expect("binance");
    assert_eq!(bn.bps.to_plain(), "-100");
}
