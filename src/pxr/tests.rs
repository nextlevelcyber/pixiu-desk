use super::*;

/// 按 `.pxr` 格式写一个内存文件。
pub(crate) fn pxr_bytes(kind: &str, records: &[(u8, i64, &str)], truncate_tail: bool) -> Vec<u8> {
    let mut out = b"PXREC1\n".to_vec();
    let head = format!(
        "{{\"version\":1,\"kind\":\"{kind}\",\"pixiu_version\":\"0.1.0\",\"config_hash\":\"0\",\"host\":\"h\",\"start_wall_ns\":0}}\n"
    );
    out.extend_from_slice(head.as_bytes());
    for (k, ts, payload) in records {
        let len = u32::try_from(RECORD_HEADER + payload.len()).expect("长度");
        out.extend_from_slice(&len.to_le_bytes());
        out.push(*k);
        out.extend_from_slice(&ts.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(payload.as_bytes());
    }
    if truncate_tail {
        out.extend_from_slice(&100u32.to_le_bytes());
        out.extend_from_slice(b"short");
    }
    out
}

fn bn(sym: &str, b: &str, a: &str) -> String {
    format!(
        "{{\"stream\":\"x@bookTicker\",\"data\":{{\"e\":\"bookTicker\",\"s\":\"{sym}\",\"b\":\"{b}\",\"B\":\"1\",\"a\":\"{a}\",\"A\":\"1\"}}}}"
    )
}

fn bg(sym: &str, b: &str, a: &str) -> String {
    format!(
        "{{\"action\":\"snapshot\",\"arg\":{{\"instType\":\"usdt-futures\",\"topic\":\"ticker\",\"symbol\":\"{sym}\"}},\"data\":[{{\"bid1Price\":\"{b}\",\"ask1Price\":\"{a}\"}}]}}"
    )
}

#[test]
fn reader_reads_records_and_counts_truncated_tail() {
    let p = bn("BTCUSDT", "1", "2");
    let bytes = pxr_bytes("binance_md", &[(2, 5, ""), (1, 7, &p)], true);
    let mut r = Reader::new(bytes.as_slice()).expect("表头");
    assert_eq!(r.header.kind, "binance_md");
    assert_eq!(r.next_rec().expect("记录").map(|x| x.kind), Some(2));
    let m = r.next_rec().expect("记录").expect("消息");
    assert_eq!((m.kind, m.recv_wall_ns), (1, 7));
    assert!(r.next_rec().expect("结束").is_none());
    assert!(r.truncated);
}

#[test]
fn reader_rejects_unknown_kind_and_bad_magic() {
    let bytes = pxr_bytes("binance_md", &[(9, 1, "")], false);
    let mut r = Reader::new(bytes.as_slice()).expect("表头");
    assert!(matches!(r.next_rec(), Err(PxrError::Corrupt(_))));
    assert!(matches!(
        Reader::new(&b"NOPE\n{}\n"[..]),
        Err(PxrError::NotPxr(_))
    ));
}

#[test]
fn decoders_pick_best_quotes_only() {
    let q =
        decode_binance(3, bn("BTCUSDT", "80713.50", "80713.60").as_bytes()).expect("bookTicker");
    assert_eq!(
        (q.symbol.as_str(), q.bid.to_plain(), q.ask.to_plain()),
        ("BTCUSDT", "80713.5".into(), "80713.6".into())
    );
    let q = decode_bitget(3, bg("BTCUSDT", "80715.4", "80715.5").as_bytes()).expect("ticker");
    assert_eq!(q.ask.to_plain(), "80715.5");
    let books = r#"{"action":"snapshot","arg":{"instType":"usdt-futures","topic":"books5","symbol":"BTCUSDT"},"data":[{"a":[["1","1"]],"b":[["1","1"]]}]}"#;
    assert!(decode_bitget(3, books.as_bytes()).is_none());
    assert!(decode_bitget(3, br#"{"event":"pong"}"#).is_none());
    assert!(
        decode_binance(3, bn("BTCUSDT", "0", "1").as_bytes()).is_none(),
        "非正价格"
    );
}

#[test]
fn sampler_emits_latest_of_both_venues_on_the_grid() {
    const S: i64 = 1_000_000_000;
    let b1 = bn("BTCUSDT", "100", "102");
    let b2 = bn("BTCUSDT", "104", "106");
    let other = bn("ETHUSDT", "1", "2");
    let g1 = bg("BTCUSDT", "101", "103");
    let fb = pxr_bytes(
        "binance_md",
        &[
            (1, 0, &b1),
            (1, S / 2, &other),
            (1, 2 * S, &b2),
            (1, 3 * S, &b2),
        ],
        false,
    );
    let fg = pxr_bytes("bitget_public", &[(1, S / 4, &g1)], false);
    let symbols: BTreeSet<String> = ["BTCUSDT".to_owned()].into();
    let sb = QuoteStream::new(Venue::Binance, vec![Box::new(fb.as_slice())], &symbols).expect("流");
    let sg = QuoteStream::new(Venue::Bitget, vec![Box::new(fg.as_slice())], &symbols).expect("流");
    let mut rows = Vec::new();
    Sampler::new(S)
        .run(sb, sg, |sym, s| rows.push((sym.to_owned(), s)))
        .expect("采样");
    let got: Vec<(i64, String, String)> = rows
        .iter()
        .map(|(_, s)| {
            (
                s.ts,
                s.binance_mid().expect("中间价").to_plain(),
                s.bitget_mid().expect("中间价").to_plain(),
            )
        })
        .collect();
    // 1 s：Binance 101、Bitget 102；2 s：同一时刻的更新算"含"；3 s 是最后一个事件，此后不再采样。
    assert_eq!(
        got,
        vec![
            (S, "101".into(), "102".into()),
            (2 * S, "105".into(), "102".into())
        ]
    );
}

#[test]
fn stream_refuses_wrong_venue() {
    let fb = pxr_bytes("binance_md", &[], false);
    let symbols = BTreeSet::new();
    assert!(QuoteStream::new(Venue::Bitget, vec![Box::new(fb.as_slice())], &symbols).is_err());
}
