//! 黄金样本：desk 的盈亏和 markout 必须与 Pixiu 的 `live_report.py --json` 逐格相等。
//!
//! 夹具 `tests/fixtures/bt-btc-2h/`：Pixiu `pixiu-backtest`（trade-tape 模型）在研究桶
//! 2026-10-28 01:00 至 03:00 UTC 的 BTCUSDT 录制上的输出（journal 只保留 desk 读的种类），
//! `live_report.json` 由 `live_report.py --journal ... --basis ... --instruments BTCUSDT --json`
//! 在同一份裁剪后的 journal 上生成。来源见 `tests/fixtures/README.md`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pixiu_desk::analysis::Params;
use pixiu_desk::report::{self, Inputs};
use serde_json::Value;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bt-btc-2h")
}

fn load() -> (Value, Value) {
    let d = dir();
    let journal = d.join("journal.jsonl");
    let basis = d.join("basis-BTCUSDT.tsv");
    let inputs = Inputs {
        journals: vec![journal.as_path()],
        basis: vec![("BTCUSDT".to_owned(), basis.as_path())],
        instruments: Some(BTreeMap::from([(0, "BTCUSDT".to_owned())])),
    };
    let ours = report::build(&inputs, &Params::default()).expect("desk 报告");
    let text = std::fs::read_to_string(d.join("live_report.json")).expect("黄金样本");
    let theirs: Value = serde_json::from_str(&text).expect("黄金样本 JSON");
    (ours, theirs)
}

#[test]
fn golden_pnl_matches_live_report() {
    let (ours, theirs) = load();
    assert_eq!(ours["pnl"]["BTCUSDT"], theirs["pnl"]["symbols"]["BTCUSDT"]);
}

#[test]
fn golden_markout_matches_live_report() {
    let (ours, theirs) = load();
    let a = &ours["markout"]["BTCUSDT"];
    let b = &theirs["markout"];
    for key in [
        "horizons_s",
        "tolerance_s",
        "missing",
        "quoting_span_s",
        "aggregate",
        "by_level",
    ] {
        assert_eq!(a[key], b[key], "markout.{key}");
    }
    let (fa, fb) = (
        a["fills"].as_array().expect("数组"),
        b["fills"].as_array().expect("数组"),
    );
    assert_eq!(fa.len(), fb.len(), "成交数");
    for (x, y) in fa.iter().zip(fb) {
        assert_eq!(x, y, "逐笔 markout（exec_id {}）", y["exec_id"]);
    }
}

#[test]
fn golden_fill_count_matches_summary() {
    let (ours, _) = load();
    let text = std::fs::read_to_string(dir().join("summary.json")).expect("summary");
    let s: Value = serde_json::from_str(&text).expect("summary JSON");
    let fills = &s["instruments"]["BTCUSDT"]["fills"];
    let n = fills["buy"].as_u64().unwrap_or(0) + fills["sell"].as_u64().unwrap_or(0);
    assert_eq!(ours["fills"].as_u64(), Some(n));
}
