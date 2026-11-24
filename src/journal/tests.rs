use super::*;

fn read(text: &str) -> Journal {
    Journal::read_all([text.as_bytes()]).expect("内存读取不会失败")
}

#[test]
fn malformed_lines_are_counted_like_live_report() {
    let j = read(concat!(
        "{\"ts_wall_ns\":5,\"kind\":\"fill\",\"side\":\"buy\"}\n",
        "not json\n",
        "{\"ts_wall_ns\":\"5\",\"kind\":\"fill\"}\n",
        "{\"ts_wall_ns\":true,\"kind\":\"fill\"}\n",
        "{\"ts_wall_ns\":6}\n",
        "[1,2]\n",
        "\n",
        "{\"ts_wall_ns\":3,\"kind\":\"venue_ticker\"}\n",
    ));
    assert_eq!(j.lines, 7);
    assert_eq!(j.malformed, 5);
    assert_eq!(j.records.len(), 1, "venue_ticker 只计数不保留");
    assert_eq!(j.first_ts, Some(3));
    assert_eq!(j.last_ts, Some(5));
    assert_eq!(j.kinds.get("venue_ticker"), Some(&1));
}

#[test]
fn records_are_sorted_stably_by_time() {
    let j = read(concat!(
        "{\"ts_wall_ns\":9,\"kind\":\"mode\",\"to\":\"a\"}\n",
        "{\"ts_wall_ns\":1,\"kind\":\"mode\",\"to\":\"b\"}\n",
        "{\"ts_wall_ns\":9,\"kind\":\"mode\",\"to\":\"c\"}\n",
    ));
    let order: Vec<_> = j.records.iter().filter_map(|r| r.str("to")).collect();
    assert_eq!(order, ["b", "a", "c"]);
}

#[test]
fn symbols_come_from_inbound_pushes() {
    let frame =
        r#"{"arg":{"topic":"order"},"data":[{"clientOid":"bdp7t02020100","symbol":"BTCUSDT"}]}"#;
    let inbound = serde_json::json!({"ts_wall_ns": 1, "kind": "inbound", "frame": frame});
    let text = format!(
        "{inbound}\n{}\n",
        r#"{"ts_wall_ns":2,"kind":"fill","instrument":3,"client_oid":"bdp7t02020100"}"#
    );
    let j = read(&text);
    let m = j.instrument_symbols(None);
    assert_eq!(m.get(&3).map(String::as_str), Some("BTCUSDT"));
}
