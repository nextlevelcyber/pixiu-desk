use super::*;
use crate::pxr::tests::pxr_bytes;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bt-btc-2h")
}

#[test]
fn backtest_dir_imports_once_and_matches_golden() {
    let mut store = Store::open_in_memory().expect("数据库");
    let opt = Options::default();
    let out = import_dir(&mut store, &golden_dir(), Some("bt-1"), &opt, 1).expect("导入");
    assert_eq!(
        out,
        Outcome::Imported {
            run_id: "bt-1".into(),
            fills: 41
        }
    );
    let again = import_dir(&mut store, &golden_dir(), Some("bt-1"), &opt, 2).expect("导入");
    assert_eq!(
        again,
        Outcome::Unchanged {
            run_id: "bt-1".into()
        }
    );

    let run = store.run("bt-1").expect("查询").expect("有这次运行");
    assert_eq!(run.source, "backtest", "由 run_start 的 mode 判断");
    assert_eq!(run.sample_ms, Some(1000));
    let text = std::fs::read_to_string(golden_dir().join("live_report.json")).expect("黄金样本");
    let golden: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(run.pnl["BTCUSDT"], golden["pnl"]["symbols"]["BTCUSDT"]);
    assert_eq!(
        run.markout["BTCUSDT"]["aggregate"],
        golden["markout"]["aggregate"]
    );
    assert!(
        run.markout["BTCUSDT"].get("fills").is_none(),
        "逐笔不进 runs 表"
    );

    let fills = store.fills("bt-1").expect("成交");
    assert_eq!(fills.len(), 41);
    let first = &golden["markout"]["fills"][0];
    assert_eq!(
        fills[0].exec_id,
        first["exec_id"].as_str().expect("exec_id")
    );
    let bps: f64 = first["horizons"]["60"]["bitget_bps"]
        .as_str()
        .expect("bps")
        .parse()
        .expect("数");
    let ours = fills[0].markout_bps["60:bitget"].as_f64().expect("bps");
    assert!((ours - bps).abs() < 0.0005, "{ours} vs {bps}");

    // 曲线终点等于窗口结束时的盈亏（同一个样本计价）。
    let curve = store.curve("bt-1", "BTCUSDT").expect("曲线");
    let last = curve.last().expect("有点");
    let total: f64 = run.pnl["BTCUSDT"]["total"]
        .as_str()
        .expect("total")
        .parse()
        .expect("数");
    assert_eq!(last.total, total);
    assert!(curve.len() > 700, "两小时、10 秒一点");
}

#[test]
fn live_dir_samples_from_pxr() {
    const S: i64 = 1_000_000_000;
    let tmp = tempfile::tempdir().expect("临时目录");
    let d = tmp.path();
    let bn = |b: &str, a: &str| {
        format!(
            "{{\"stream\":\"btcusdt@bookTicker\",\"data\":{{\"s\":\"BTCUSDT\",\"b\":\"{b}\",\"a\":\"{a}\"}}}}"
        )
    };
    let bg = |b: &str, a: &str| {
        format!(
            "{{\"action\":\"snapshot\",\"arg\":{{\"topic\":\"ticker\",\"symbol\":\"BTCUSDT\"}},\"data\":[{{\"bid1Price\":\"{b}\",\"ask1Price\":\"{a}\"}}]}}"
        )
    };
    let (b0, b1) = (bn("100", "100"), bn("110", "110"));
    let (g0, g1) = (bg("100", "100"), bg("112", "112"));
    let base = 1_000 * S;
    std::fs::write(
        d.join("binance-md-20261028T01-000.pxr"),
        pxr_bytes(
            "binance_md",
            &[
                (1, base, &b0),
                (1, base + 3 * S, &b1),
                (1, base + 4 * S, &b1),
            ],
            false,
        ),
    )
    .expect("写");
    std::fs::write(
        d.join("bitget-public-20261028T01-000.pxr"),
        pxr_bytes(
            "bitget_public",
            &[(1, base, &g0), (1, base + 3 * S, &g1)],
            false,
        ),
    )
    .expect("写");
    let fill = json!({"ts_wall_ns": base + S, "kind": "fill", "instrument": 0, "exec_id_hash": 9,
                      "price_mantissa": 10_000_000_000i64, "qty_mantissa": 100_000_000,
                      "side": "buy", "client_oid": "bdp5t02020100"});
    let start =
        json!({"ts_wall_ns": base, "kind": "run_start", "mode": "live", "config_hash": "abc"});
    std::fs::write(
        d.join("journal-20261028-000.jsonl"),
        format!("{start}\n{fill}\n"),
    )
    .expect("写");
    std::fs::write(
        d.join("desk-meta.json"),
        r#"{"label":"测试","instruments":["BTCUSDT"]}"#,
    )
    .expect("写");

    let mut store = Store::open_in_memory().expect("数据库");
    let out = import_dir(&mut store, d, Some("live-1"), &Options::default(), 1).expect("导入");
    assert_eq!(
        out,
        Outcome::Imported {
            run_id: "live-1".into(),
            fills: 1
        }
    );
    let run = store.run("live-1").expect("查询").expect("运行");
    assert_eq!(run.source, "live");
    assert_eq!(run.label.as_deref(), Some("测试"));
    assert_eq!(run.sample_ms, Some(100));
    let f = &store.fills("live-1").expect("成交")[0];
    // 2 s 后（base + 3 s）Bitget 中间价 112：买入 100 后 +1200 bps。
    assert_eq!(f.markout_bps["2:bitget"].as_f64(), Some(1200.0));
    assert_eq!(f.markout_bps["2:binance"].as_f64(), Some(1000.0));
    assert_eq!(f.level, Some(0));
}

#[test]
fn dir_without_journal_is_refused() {
    let tmp = tempfile::tempdir().expect("临时目录");
    let mut store = Store::open_in_memory().expect("数据库");
    assert!(import_dir(&mut store, tmp.path(), None, &Options::default(), 1).is_err());
}

#[test]
fn golden_breakdown_sums_to_total_and_checks_pass() {
    let mut store = Store::open_in_memory().expect("数据库");
    import_dir(
        &mut store,
        &golden_dir(),
        Some("bt-1"),
        &Options::default(),
        1,
    )
    .expect("导入");
    let run = store.run("bt-1").expect("查询").expect("运行");
    let b = &run.breakdown.expect("有拆分")["BTCUSDT"];
    let fx = |k: &str| crate::fx::Fx::parse(b[k].as_str().expect("字符串")).expect("数");
    let sum = fx("edge")
        .plus(fx("adverse"))
        .and_then(|x| x.plus(fx("drift")))
        .and_then(|x| x.minus(fx("fees")))
        .expect("合计");
    assert_eq!(
        sum.to_plain(),
        run.pnl["BTCUSDT"]["total"].as_str().expect("total")
    );
    assert_eq!(b["fills"], 41);
    // 0 秒价差收益 = live_report 的 0 秒 Bitget quote_sum。
    let text = std::fs::read_to_string(golden_dir().join("live_report.json")).expect("黄金样本");
    let golden: Value = serde_json::from_str(&text).expect("JSON");
    let q0 = crate::fx::Fx::parse(
        golden["markout"]["aggregate"]["0"]["bitget"]["quote_sum"]
            .as_str()
            .expect("quote_sum"),
    )
    .expect("数");
    assert_eq!(fx("edge").round_to(6).expect("舍入"), q0);

    let checks = run.checks.expect("有核对");
    let warns: Vec<String> = checks
        .as_array()
        .expect("数组")
        .iter()
        .flat_map(|g| g["items"].as_array().cloned().unwrap_or_default())
        .filter(|i| i["verdict"] == "warn")
        .map(|i| format!("{} = {}", i["name"], i["value"]))
        .collect();
    // 回测运行末尾的成交缺长 horizon 样本（7 格），以及运行结束时仍未终结的订单没有手续费，属正常的提示。
    assert!(
        warns
            .iter()
            .all(|w| w.contains("markout 缺格") || w.contains("没有手续费")),
        "{warns:?}"
    );
}

#[test]
fn schema_v1_database_is_upgraded() {
    let tmp = tempfile::tempdir().expect("临时目录");
    let path = tmp.path().join("old.sqlite");
    {
        let c = rusqlite::Connection::open(&path).expect("打开");
        c.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES ('schema', '1');
             CREATE TABLE runs (run_id TEXT PRIMARY KEY, source TEXT NOT NULL, label TEXT,
               started_ns INTEGER, stopped_ns INTEGER, config_hash TEXT, version TEXT, mode TEXT,
               fingerprint TEXT NOT NULL, fills INTEGER NOT NULL, symbols TEXT NOT NULL,
               pnl TEXT NOT NULL, markout TEXT NOT NULL, summary TEXT, meta TEXT, sample_ms INTEGER,
               imported_ns INTEGER NOT NULL);
             INSERT INTO runs VALUES ('old', 'live', NULL, 1, 2, NULL, NULL, NULL, 'f', 0, '{}',
               '{}', '{}', NULL, NULL, NULL, 3);",
        )
        .expect("建第 1 版");
    }
    let mut store = Store::open(&path).expect("升级");
    let old = store.run("old").expect("查询").expect("旧运行还在");
    assert!(old.breakdown.is_none() && old.checks.is_none());
    import_dir(
        &mut store,
        &golden_dir(),
        Some("bt-1"),
        &Options::default(),
        1,
    )
    .expect("导入");
    assert!(
        store
            .run("bt-1")
            .expect("查询")
            .expect("运行")
            .breakdown
            .is_some()
    );
}
