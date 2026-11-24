//! Pixiu journal（JSON lines，Pixiu spec 12.5）的读取。
//!
//! 只保留 desk 用到的记录种类，其余只计数，这样一天的实盘 journal 也能在小机型上读完。
//! 判定规则与 `live_report.py` 的 `read_journal` 一致：一行必须是 JSON 对象，`ts_wall_ns` 是整数、
//! `kind` 是字符串，否则计为 malformed。读完后按 `ts_wall_ns` 稳定排序。

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;

use serde::Deserialize;
use serde_json::Value;

/// desk 需要完整保留的记录种类。
const KEPT: &[&str] = &[
    "run_start",
    "run_stop",
    "mode",
    "order_registered",
    "fill",
    "order_fee",
    "booked",
    "position_snapshot",
    "account_snapshot",
];

/// 一条保留下来的记录。
#[derive(Debug, Clone)]
pub struct Record {
    pub ts: i64,
    pub kind: String,
    pub v: Value,
}

impl Record {
    pub fn int(&self, key: &str) -> Option<i64> {
        int_of(self.v.get(key))
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.v.get(key).and_then(Value::as_str)
    }
}

/// JSON 整数（不接受布尔和小数），与 Python 的 `isinstance(x, int) and not isinstance(x, bool)` 一致。
pub fn int_of(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

/// 读完的 journal。
#[derive(Debug, Default)]
pub struct Journal {
    /// 保留的记录，按时间稳定排序。
    pub records: Vec<Record>,
    /// 全部合法记录的最早、最晚 `ts_wall_ns`（窗口的起止）。
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    /// 非空行数、malformed 行数、各种类的计数。
    pub lines: u64,
    pub malformed: u64,
    pub kinds: BTreeMap<String, u64>,
    /// 从 `inbound` 私有推送里看到的 clientOid 到交易对的映射（实盘 journal 用来确定合约名）。
    pub oid_symbol: HashMap<String, String>,
}

#[derive(Deserialize)]
struct Head {
    #[serde(default)]
    ts_wall_ns: Value,
    #[serde(default)]
    kind: Value,
}

impl Journal {
    /// 依次读入多个文件（顺序与命令行一致），最后统一排序。
    pub fn read_all<R: BufRead>(readers: impl IntoIterator<Item = R>) -> std::io::Result<Journal> {
        let mut j = Journal::default();
        for r in readers {
            j.read_one(r)?;
        }
        j.records.sort_by_key(|r| r.ts);
        Ok(j)
    }

    fn read_one<R: BufRead>(&mut self, reader: R) -> std::io::Result<()> {
        for line in reader.split(b'\n') {
            let bytes = line?;
            // 与 Python 的 errors="replace" 一致：非法 UTF-8 不中断读取。
            let text = String::from_utf8_lossy(&bytes);
            let text = text.trim_end_matches('\r');
            if text.trim().is_empty() {
                continue;
            }
            self.lines += 1;
            let Ok(head) = serde_json::from_str::<Head>(text) else {
                self.malformed += 1;
                continue;
            };
            let (Some(ts), Some(kind)) = (int_of(Some(&head.ts_wall_ns)), head.kind.as_str())
            else {
                self.malformed += 1;
                continue;
            };
            *self.kinds.entry(kind.to_owned()).or_default() += 1;
            self.first_ts = Some(self.first_ts.map_or(ts, |t| t.min(ts)));
            self.last_ts = Some(self.last_ts.map_or(ts, |t| t.max(ts)));
            if kind == "inbound" {
                self.note_inbound(text);
                continue;
            }
            if !KEPT.contains(&kind) {
                continue;
            }
            match serde_json::from_str::<Value>(text) {
                Ok(v) => self.records.push(Record {
                    ts,
                    kind: kind.to_owned(),
                    v,
                }),
                Err(_) => self.malformed += 1,
            }
        }
        Ok(())
    }

    /// `inbound` 记录的 `frame` 是私有 WS 的原始帧；从推送元素里收集 clientOid 和 symbol。
    fn note_inbound(&mut self, text: &str) {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let Some(frame) = v.get("frame").and_then(Value::as_str) else {
            return;
        };
        let Ok(f) = serde_json::from_str::<Value>(frame) else {
            return;
        };
        if f.get("event").is_some() {
            return;
        }
        let elements: Vec<&Value> = match f.get("data") {
            Some(Value::Array(a)) => a.iter().collect(),
            Some(o @ Value::Object(_)) => vec![o],
            _ => Vec::new(),
        };
        for el in elements {
            if let (Some(oid), Some(sym)) = (
                el.get("clientOid").and_then(Value::as_str),
                el.get("symbol").and_then(Value::as_str),
            ) {
                self.oid_symbol
                    .entry(oid.to_owned())
                    .or_insert_with(|| sym.to_owned());
            }
        }
    }

    pub fn of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Record> + 'a {
        self.records.iter().filter(move |r| r.kind == kind)
    }

    /// 合约序号到交易对：优先用调用方给的映射（回测的 summary.json、配置），否则从 inbound 推导
    /// （与 `live_report.py` 的 `instrument_symbols` 一致）。
    pub fn instrument_symbols(
        &self,
        given: Option<&BTreeMap<i64, String>>,
    ) -> BTreeMap<i64, String> {
        if let Some(g) = given {
            return g.clone();
        }
        let mut out = BTreeMap::new();
        for kind in ["order_registered", "fill", "order_fee"] {
            for r in self.of_kind(kind) {
                if let (Some(instr), Some(oid)) = (r.int("instrument"), r.str("client_oid"))
                    && let Some(sym) = self.oid_symbol.get(oid)
                {
                    out.entry(instr).or_insert_with(|| sym.clone());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
