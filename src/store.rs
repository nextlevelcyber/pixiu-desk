//! SQLite 存储（单文件）。
//!
//! 每次运行（实盘或回测）一行 `runs`，加上它的成交、markout 格和累计盈亏曲线。导入以"整次运行"为单位
//! 替换：先删再写，在一个事务里完成，所以同一次运行重复导入不会留下重复行。金额一律以十进制字符串
//! 保存（精确），图表需要的数值在查询时转换。

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::analysis::{CurvePoint, Fill, MarkoutCell};

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runs (
    run_id       TEXT PRIMARY KEY,
    source       TEXT NOT NULL,          -- live | backtest
    label        TEXT,
    started_ns   INTEGER,
    stopped_ns   INTEGER,
    config_hash  TEXT,
    version      TEXT,
    mode         TEXT,
    fingerprint  TEXT NOT NULL,          -- 输入文件的 sha256
    fills        INTEGER NOT NULL,
    symbols      TEXT NOT NULL,          -- JSON：合约序号 -> 交易对
    pnl          TEXT NOT NULL,          -- JSON：交易对 -> 盈亏行
    markout      TEXT NOT NULL,          -- JSON：交易对 -> markout 报告（不含逐笔）
    summary      TEXT,                   -- 回测的 summary.json 原文
    meta         TEXT,                   -- desk-meta.json 原文
    sample_ms    INTEGER,                -- markout 样本的间隔
    imported_ns  INTEGER NOT NULL,
    breakdown    TEXT,                   -- JSON：交易对 -> 收益拆分（第 2 版起）
    checks       TEXT                    -- JSON：数据核对的分组（第 2 版起）
);
CREATE TABLE IF NOT EXISTS fills (
    run_id     TEXT NOT NULL,
    exec_id    TEXT NOT NULL,
    ts_ns      INTEGER NOT NULL,
    symbol     TEXT NOT NULL,
    side       TEXT NOT NULL,
    price      TEXT NOT NULL,
    qty        TEXT NOT NULL,
    level      INTEGER,
    scope      TEXT,
    client_oid TEXT,
    PRIMARY KEY (run_id, exec_id)
);
CREATE INDEX IF NOT EXISTS fills_by_time ON fills (run_id, ts_ns);
CREATE TABLE IF NOT EXISTS markouts (
    run_id     TEXT NOT NULL,
    exec_id    TEXT NOT NULL,
    horizon_s  INTEGER NOT NULL,
    reference  TEXT NOT NULL,            -- bitget | binance
    sample_ns  INTEGER NOT NULL,
    bps        TEXT NOT NULL,
    quote      TEXT NOT NULL,
    PRIMARY KEY (run_id, exec_id, horizon_s, reference)
);
CREATE TABLE IF NOT EXISTS curve (
    run_id     TEXT NOT NULL,
    symbol     TEXT NOT NULL,
    ts_ns      INTEGER NOT NULL,
    position   TEXT NOT NULL,
    realised   TEXT NOT NULL,
    unrealised TEXT NOT NULL,
    fees       TEXT NOT NULL,
    total      TEXT NOT NULL,
    mid        TEXT NOT NULL,
    PRIMARY KEY (run_id, symbol, ts_ns)
);
";

/// 数据库句柄。
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

/// 一次运行写入存储所需的全部内容。
#[derive(Debug)]
pub struct RunData<'a> {
    pub run_id: &'a str,
    pub source: &'a str,
    pub label: Option<&'a str>,
    pub started_ns: Option<i64>,
    pub stopped_ns: Option<i64>,
    pub config_hash: Option<&'a str>,
    pub version: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub fingerprint: &'a str,
    pub symbols: Value,
    pub pnl: Value,
    pub markout: Value,
    pub summary: Option<&'a str>,
    pub meta: Option<&'a str>,
    pub sample_ms: Option<i64>,
    pub breakdown: Value,
    pub checks: Value,
    pub fills: &'a [Fill],
    pub markouts: &'a [(String, MarkoutCell)],
    pub curves: &'a [(String, Vec<CurvePoint>)],
}

/// 运行列表的一行。
#[derive(Debug, Clone, Serialize)]
pub struct RunRow {
    pub run_id: String,
    pub source: String,
    pub label: Option<String>,
    pub started_ns: Option<i64>,
    pub stopped_ns: Option<i64>,
    pub config_hash: Option<String>,
    pub version: Option<String>,
    pub mode: Option<String>,
    pub fills: i64,
    pub symbols: Value,
    pub pnl: Value,
    pub markout: Value,
    pub summary: Option<Value>,
    pub meta: Option<Value>,
    pub sample_ms: Option<i64>,
    pub imported_ns: i64,
    pub breakdown: Option<Value>,
    pub checks: Option<Value>,
}

/// 一笔成交及其 markout（网页的成交表）。
#[derive(Debug, Clone, Serialize)]
pub struct FillRow {
    pub exec_id: String,
    pub ts_ns: i64,
    pub symbol: String,
    pub side: String,
    pub price: String,
    pub qty: String,
    pub level: Option<i64>,
    pub scope: Option<String>,
    /// "<horizon>:<reference>" -> bps
    pub markout_bps: serde_json::Map<String, Value>,
}

/// 曲线上的点（数值已转成浮点，供图表使用）。
#[derive(Debug, Clone, Serialize)]
pub struct CurveRow {
    pub ts_ns: i64,
    pub position: f64,
    pub realised: f64,
    pub unrealised: f64,
    pub fees: f64,
    pub total: f64,
    pub mid: f64,
}

fn parse_json(text: Option<String>) -> Option<Value> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

fn num(text: &str) -> f64 {
    // 仅用于图表输出。
    text.parse::<f64>().unwrap_or(f64::NAN)
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn =
            Connection::open(path).with_context(|| format!("打不开数据库 {}", path.display()))?;
        Store::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch(SCHEMA)?;
        let v: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match v {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema', ?1)",
                    [SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(v) if v == SCHEMA_VERSION.to_string() => {}
            Some(v) if v == "1" => {
                // 第 1 版 → 第 2 版：加两列；旧运行的这两列为空，重新导入（--force）后补齐。
                conn.execute_batch(
                    "BEGIN;
                     ALTER TABLE runs ADD COLUMN breakdown TEXT;
                     ALTER TABLE runs ADD COLUMN checks TEXT;
                     UPDATE meta SET value = '2' WHERE key = 'schema';
                     COMMIT;",
                )?;
            }
            Some(v) => anyhow::bail!("数据库 schema 版本是 {v}，本程序需要 {SCHEMA_VERSION}"),
        }
        Ok(Store { conn })
    }

    /// 已导入的同一次运行的指纹。
    pub fn fingerprint(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT fingerprint FROM runs WHERE run_id = ?1",
                [run_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 写入（替换）一次运行。
    pub fn replace_run(&mut self, d: &RunData<'_>, imported_ns: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        for table in ["runs", "fills", "markouts", "curve"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE run_id = ?1"),
                [d.run_id],
            )?;
        }
        tx.execute(
            "INSERT INTO runs (run_id, source, label, started_ns, stopped_ns, config_hash, version,
                               mode, fingerprint, fills, symbols, pnl, markout, summary, meta,
                               sample_ms, imported_ns, breakdown, checks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18, ?19)",
            params![
                d.run_id,
                d.source,
                d.label,
                d.started_ns,
                d.stopped_ns,
                d.config_hash,
                d.version,
                d.mode,
                d.fingerprint,
                i64::try_from(d.fills.len()).unwrap_or(i64::MAX),
                d.symbols.to_string(),
                d.pnl.to_string(),
                d.markout.to_string(),
                d.summary,
                d.meta,
                d.sample_ms,
                imported_ns,
                d.breakdown.to_string(),
                d.checks.to_string(),
            ],
        )?;
        {
            let mut st = tx.prepare(
                "INSERT INTO fills (run_id, exec_id, ts_ns, symbol, side, price, qty, level, scope,
                                    client_oid) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for f in d.fills {
                st.execute(params![
                    d.run_id,
                    f.exec_id,
                    f.ts,
                    f.symbol,
                    f.side.as_str(),
                    f.price.to_plain(),
                    f.qty.to_plain(),
                    f.level,
                    f.scope,
                    f.client_oid
                ])?;
            }
            let mut st = tx.prepare(
                "INSERT INTO markouts (run_id, exec_id, horizon_s, reference, sample_ns, bps, quote)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (exec_id, c) in d.markouts {
                st.execute(params![
                    d.run_id,
                    exec_id,
                    c.horizon_s,
                    c.reference.key(),
                    c.sample_ts,
                    c.bps.fixed_unsigned_zero(6)?,
                    c.quote.to_plain()
                ])?;
            }
            let mut st = tx.prepare(
                "INSERT INTO curve (run_id, symbol, ts_ns, position, realised, unrealised, fees,
                                    total, mid) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (symbol, points) in d.curves {
                for p in points {
                    st.execute(params![
                        d.run_id,
                        symbol,
                        p.ts,
                        p.position.to_plain(),
                        p.realised.to_plain(),
                        p.unrealised.to_plain(),
                        p.fees.to_plain(),
                        p.total.to_plain(),
                        p.mid.to_plain()
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn run_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
        Ok(RunRow {
            run_id: r.get(0)?,
            source: r.get(1)?,
            label: r.get(2)?,
            started_ns: r.get(3)?,
            stopped_ns: r.get(4)?,
            config_hash: r.get(5)?,
            version: r.get(6)?,
            mode: r.get(7)?,
            fills: r.get(8)?,
            symbols: parse_json(r.get(9)?).unwrap_or(Value::Null),
            pnl: parse_json(r.get(10)?).unwrap_or(Value::Null),
            markout: parse_json(r.get(11)?).unwrap_or(Value::Null),
            summary: parse_json(r.get(12)?),
            meta: parse_json(r.get(13)?),
            sample_ms: r.get(14)?,
            imported_ns: r.get(15)?,
            breakdown: parse_json(r.get(16)?),
            checks: parse_json(r.get(17)?),
        })
    }

    const RUN_COLUMNS: &'static str = "run_id, source, label, started_ns, stopped_ns, config_hash,
        version, mode, fills, symbols, pnl, markout, summary, meta, sample_ms, imported_ns,
        breakdown, checks";

    /// 全部运行，最新的在前。
    pub fn runs(&self) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {} FROM runs ORDER BY COALESCE(started_ns, 0) DESC, run_id",
            Store::RUN_COLUMNS
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], Store::run_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn run(&self, run_id: &str) -> Result<Option<RunRow>> {
        let sql = format!("SELECT {} FROM runs WHERE run_id = ?1", Store::RUN_COLUMNS);
        Ok(self
            .conn
            .query_row(&sql, [run_id], Store::run_from_row)
            .optional()?)
    }

    /// 一次运行的成交及各格 markout（bps）。
    pub fn fills(&self, run_id: &str) -> Result<Vec<FillRow>> {
        let mut st = self.conn.prepare(
            "SELECT exec_id, ts_ns, symbol, side, price, qty, level, scope FROM fills
             WHERE run_id = ?1 ORDER BY ts_ns, exec_id",
        )?;
        let mut out: Vec<FillRow> = st
            .query_map([run_id], |r| {
                Ok(FillRow {
                    exec_id: r.get(0)?,
                    ts_ns: r.get(1)?,
                    symbol: r.get(2)?,
                    side: r.get(3)?,
                    price: r.get(4)?,
                    qty: r.get(5)?,
                    level: r.get(6)?,
                    scope: r.get(7)?,
                    markout_bps: serde_json::Map::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let index: std::collections::HashMap<String, usize> = out
            .iter()
            .enumerate()
            .map(|(i, f)| (f.exec_id.clone(), i))
            .collect();
        let mut st = self
            .conn
            .prepare("SELECT exec_id, horizon_s, reference, bps FROM markouts WHERE run_id = ?1")?;
        let rows = st.query_map([run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (exec_id, h, reference, bps) = row?;
            if let Some(&i) = index.get(&exec_id) {
                out[i]
                    .markout_bps
                    .insert(format!("{h}:{reference}"), Value::from(num(&bps)));
            }
        }
        Ok(out)
    }

    /// 一次运行一个合约的累计盈亏曲线。
    pub fn curve(&self, run_id: &str, symbol: &str) -> Result<Vec<CurveRow>> {
        let mut st = self.conn.prepare(
            "SELECT ts_ns, position, realised, unrealised, fees, total, mid FROM curve
             WHERE run_id = ?1 AND symbol = ?2 ORDER BY ts_ns",
        )?;
        let rows = st.query_map(params![run_id, symbol], |r| {
            Ok(CurveRow {
                ts_ns: r.get(0)?,
                position: num(&r.get::<_, String>(1)?),
                realised: num(&r.get::<_, String>(2)?),
                unrealised: num(&r.get::<_, String>(3)?),
                fees: num(&r.get::<_, String>(4)?),
                total: num(&r.get::<_, String>(5)?),
                mid: num(&r.get::<_, String>(6)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_run(&mut self, run_id: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for table in ["runs", "fills", "markouts", "curve"] {
            n += tx.execute(&format!("DELETE FROM {table} WHERE run_id = ?1"), [run_id])?;
        }
        tx.commit()?;
        Ok(n > 0)
    }
}
