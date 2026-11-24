use super::*;

#[test]
fn basis_tsv_with_header() {
    let text = "ts\tbitget_bid\tbitget_ask\tbinance_bid\tbinance_ask\n\
                1793149202007890853\t80716.9\t80717\t80717.9\t80718\n\
                1793149201007890853\t80718.8\t80718.9\t80719.9\t80720\n\
                bad\t1\t2\t3\t4\n\
                1793149203007890853\t0\t1\t1\t1\n";
    let r = read_basis_tsv(text.as_bytes()).expect("读取");
    assert_eq!(r.malformed, 2);
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].ts, 1793149201007890853, "按时间排序");
    assert_eq!(
        r.rows[0].bitget_mid().expect("中间价").to_plain(),
        "80718.85"
    );
    assert_eq!(
        r.rows[0].binance_mid().expect("中间价").to_plain(),
        "80719.95"
    );
}

#[test]
fn ts_magnitudes() {
    assert_eq!(parse_ts("1793149201"), Some(1_793_149_201_000_000_000));
    assert_eq!(parse_ts("1793149201007"), Some(1_793_149_201_007_000_000));
    assert_eq!(
        parse_ts("1793149201007890"),
        Some(1_793_149_201_007_890_000)
    );
    assert_eq!(
        parse_ts("1793149201007890853"),
        Some(1_793_149_201_007_890_853)
    );
    assert_eq!(parse_ts("1793149201.5"), Some(1_793_149_201_500_000_000));
}
