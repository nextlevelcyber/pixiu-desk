//! Pixiu `.pxr` 录制的读取（Pixiu spec 4.6、`crates/md/src/record.rs`、`scripts/research/pxr.py`）。
//!
//! 文件格式：第一行 `PXREC1`，第二行是 JSON 表头（`version`、`kind`、`pixiu_version`、`config_hash`、
//! `host`、`start_wall_ns`），之后是小端序记录：`u32 len`（其后的字节数）、`u8 kind`（1 消息、2 已连接、
//! 3 已断开、4 丢弃标记）、`i64 recv_wall_ns`、`u64 t0_mono_ns`、`u32 conn_epoch`、负载。
//! 文件末尾被截断的记录（崩溃或 gzip 流不完整）结束读取并计数，不算错误；完整记录短于 21 字节头部，
//! 或 kind 未知，算损坏。
//!
//! 只解码 desk 需要的两种行情：Binance `bookTicker` 和 Bitget `ticker`（最优买卖价）。

use std::collections::BTreeSet;
use std::io::{BufRead, Read};

use serde::Deserialize;

use crate::fx::Fx;
use crate::samples::Sample;

const MAGIC: &[u8] = b"PXREC1";
const RECORD_HEADER: usize = 21;

/// 表头。
#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub version: u32,
    pub kind: String,
    #[serde(default)]
    pub pixiu_version: String,
    #[serde(default)]
    pub config_hash: String,
    #[serde(default)]
    pub start_wall_ns: i64,
}

/// 一条记录。
#[derive(Debug, Clone)]
pub struct Rec {
    pub kind: u8,
    pub recv_wall_ns: i64,
    pub payload: Vec<u8>,
}

/// 读取错误。
#[derive(Debug)]
pub enum PxrError {
    Io(std::io::Error),
    NotPxr(String),
    Corrupt(String),
}

impl std::fmt::Display for PxrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PxrError::Io(e) => write!(f, "读取失败：{e}"),
            PxrError::NotPxr(m) => write!(f, "不是 .pxr 文件：{m}"),
            PxrError::Corrupt(m) => write!(f, ".pxr 损坏：{m}"),
        }
    }
}

impl std::error::Error for PxrError {}

impl From<std::io::Error> for PxrError {
    fn from(e: std::io::Error) -> PxrError {
        PxrError::Io(e)
    }
}

/// 逐条读取记录。
#[derive(Debug)]
pub struct Reader<R> {
    inner: R,
    pub header: Header,
    /// 文件末尾是否有被截断的记录。
    pub truncated: bool,
    done: bool,
}

/// 读满 `buf`；返回实际读到的字节数（文件结束时可能不足）。
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // 截断的 gzip 流报 UnexpectedEof：当作文件结束。
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

impl<R: BufRead> Reader<R> {
    pub fn new(mut inner: R) -> Result<Reader<R>, PxrError> {
        let mut magic = Vec::new();
        inner.read_until(b'\n', &mut magic)?;
        if magic.strip_suffix(b"\n").unwrap_or(&magic) != MAGIC {
            return Err(PxrError::NotPxr("缺少 PXREC1".into()));
        }
        let mut head = Vec::new();
        inner.read_until(b'\n', &mut head)?;
        let header: Header = serde_json::from_slice(&head)
            .map_err(|e| PxrError::NotPxr(format!("表头不是 JSON：{e}")))?;
        Ok(Reader {
            inner,
            header,
            truncated: false,
            done: false,
        })
    }

    /// 下一条记录；文件结束返回 `None`。
    pub fn next_rec(&mut self) -> Result<Option<Rec>, PxrError> {
        if self.done {
            return Ok(None);
        }
        let mut len = [0u8; 4];
        let n = read_full(&mut self.inner, &mut len)?;
        if n == 0 {
            self.done = true;
            return Ok(None);
        }
        if n < 4 {
            self.truncated = true;
            self.done = true;
            return Ok(None);
        }
        let len = u32::from_le_bytes(len) as usize;
        let mut body = vec![0u8; len];
        if read_full(&mut self.inner, &mut body)? < len {
            self.truncated = true;
            self.done = true;
            return Ok(None);
        }
        if len < RECORD_HEADER {
            return Err(PxrError::Corrupt(format!("记录长度 {len} 小于头部")));
        }
        let kind = body[0];
        if !(1..=4).contains(&kind) {
            return Err(PxrError::Corrupt(format!("未知的记录种类 {kind}")));
        }
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&body[1..9]);
        let payload = body.split_off(RECORD_HEADER);
        Ok(Some(Rec {
            kind,
            recv_wall_ns: i64::from_le_bytes(ts),
            payload,
        }))
    }
}

/// 一次最优价更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub ts: i64,
    pub symbol: String,
    pub bid: Fx,
    pub ask: Fx,
}

#[derive(Deserialize)]
struct BinanceMsg<'a> {
    #[serde(borrow)]
    data: Option<BinanceData<'a>>,
}

#[derive(Deserialize)]
struct BinanceData<'a> {
    #[serde(borrow)]
    s: &'a str,
    #[serde(borrow)]
    b: &'a str,
    #[serde(borrow)]
    a: &'a str,
}

#[derive(Deserialize)]
struct BitgetMsg<'a> {
    #[serde(borrow, default)]
    arg: Option<BitgetArg<'a>>,
    #[serde(borrow, default)]
    data: Option<Vec<BitgetTicker<'a>>>,
}

#[derive(Deserialize)]
struct BitgetArg<'a> {
    #[serde(borrow, default)]
    topic: Option<&'a str>,
    #[serde(borrow, default)]
    symbol: Option<&'a str>,
}

#[derive(Deserialize)]
struct BitgetTicker<'a> {
    #[serde(borrow, rename = "bid1Price", default)]
    bid: Option<&'a str>,
    #[serde(borrow, rename = "ask1Price", default)]
    ask: Option<&'a str>,
}

fn quote(ts: i64, symbol: &str, bid: &str, ask: &str) -> Option<Quote> {
    let (bid, ask) = (Fx::parse(bid).ok()?, Fx::parse(ask).ok()?);
    (bid.signum() > 0 && ask.signum() > 0).then(|| Quote {
        ts,
        symbol: symbol.to_owned(),
        bid,
        ask,
    })
}

/// Binance `bookTicker` 消息（组合流格式 `{"stream":..,"data":{"s","b","a",..}}`）。
pub fn decode_binance(ts: i64, payload: &[u8]) -> Option<Quote> {
    let m: BinanceMsg<'_> = serde_json::from_slice(payload).ok()?;
    let d = m.data?;
    quote(ts, d.s, d.b, d.a)
}

/// Bitget `ticker` 推送的最优买卖价；其他频道（books5、publicTrade）和事件返回 `None`。
pub fn decode_bitget(ts: i64, payload: &[u8]) -> Option<Quote> {
    // 先用便宜的子串判断跳过非 ticker 帧（books5 占了大部分字节）。
    if !contains(payload, b"\"ticker\"") {
        return None;
    }
    let m: BitgetMsg<'_> = serde_json::from_slice(payload).ok()?;
    let arg = m.arg?;
    if arg.topic? != "ticker" {
        return None;
    }
    let t = m.data?.into_iter().next()?;
    quote(ts, arg.symbol?, t.bid?, t.ask?)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// 一个 venue 的报价流：按顺序读多个文件（同一进程、时间顺序）。
pub struct QuoteStream<'a> {
    readers: Vec<Reader<Box<dyn BufRead + 'a>>>,
    decode: fn(i64, &[u8]) -> Option<Quote>,
    symbols: &'a BTreeSet<String>,
    pub messages: u64,
    pub decoded: u64,
    pub truncated_files: u64,
}

impl std::fmt::Debug for QuoteStream<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuoteStream")
            .field("files", &self.readers.len())
            .field("messages", &self.messages)
            .field("decoded", &self.decoded)
            .finish()
    }
}

/// 录制的 venue。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Venue {
    Binance,
    Bitget,
}

impl Venue {
    /// 由表头的 `kind` 判断。
    pub fn of_kind(kind: &str) -> Option<Venue> {
        match kind {
            "binance_md" => Some(Venue::Binance),
            "bitget_public" => Some(Venue::Bitget),
            _ => None,
        }
    }
}

impl<'a> QuoteStream<'a> {
    pub fn new(
        venue: Venue,
        files: Vec<Box<dyn BufRead + 'a>>,
        symbols: &'a BTreeSet<String>,
    ) -> Result<QuoteStream<'a>, PxrError> {
        let mut readers = Vec::new();
        for f in files {
            let r = Reader::new(f)?;
            if Venue::of_kind(&r.header.kind) != Some(venue) {
                return Err(PxrError::NotPxr(format!(
                    "期望 {venue:?} 录制，表头是 {}",
                    r.header.kind
                )));
            }
            readers.push(r);
        }
        readers.reverse(); // 从尾部弹出
        Ok(QuoteStream {
            readers,
            decode: match venue {
                Venue::Binance => decode_binance,
                Venue::Bitget => decode_bitget,
            },
            symbols,
            messages: 0,
            decoded: 0,
            truncated_files: 0,
        })
    }

    /// 下一个属于关注合约的报价。
    pub fn next_quote(&mut self) -> Result<Option<Quote>, PxrError> {
        loop {
            let Some(r) = self.readers.last_mut() else {
                return Ok(None);
            };
            match r.next_rec()? {
                None => {
                    if r.truncated {
                        self.truncated_files += 1;
                    }
                    self.readers.pop();
                }
                Some(rec) if rec.kind == 1 => {
                    self.messages += 1;
                    if let Some(q) = (self.decode)(rec.recv_wall_ns, &rec.payload)
                        && self.symbols.contains(&q.symbol)
                    {
                        self.decoded += 1;
                        return Ok(Some(q));
                    }
                }
                Some(_) => {}
            }
        }
    }
}

/// 联合采样：从 `start` 起每隔 `interval_ns` 取一个时刻，输出每个合约在该时刻（含）之前两个 venue 的
/// 最新报价；某个 venue 还没有报价的合约不输出。与 Pixiu 回测的 basis 采样同一口径
/// （起点为两个流中最早的记录时间，第一个样本在起点之后一个间隔）。
#[derive(Debug)]
pub struct Sampler {
    pub interval_ns: i64,
    latest_bg: std::collections::BTreeMap<String, (Fx, Fx)>,
    latest_bn: std::collections::BTreeMap<String, (Fx, Fx)>,
}

impl Sampler {
    pub fn new(interval_ns: i64) -> Sampler {
        Sampler {
            interval_ns,
            latest_bg: Default::default(),
            latest_bn: Default::default(),
        }
    }

    /// 跑完两个流，每个样本交给 `emit(symbol, sample)`。
    pub fn run(
        &mut self,
        mut binance: QuoteStream<'_>,
        mut bitget: QuoteStream<'_>,
        mut emit: impl FnMut(&str, Sample),
    ) -> Result<(), PxrError> {
        let mut nb = binance.next_quote()?;
        let mut ng = bitget.next_quote()?;
        let start = match (nb.as_ref(), ng.as_ref()) {
            (Some(a), Some(b)) => a.ts.min(b.ts),
            (Some(a), None) => a.ts,
            (None, Some(b)) => b.ts,
            (None, None) => return Ok(()),
        };
        let mut next_tick = start + self.interval_ns;
        loop {
            // 下一个事件：较早的报价（同一时刻 Binance 优先，与顺序无关，因为采样取"含"该时刻）。
            let next_ts = match (nb.as_ref(), ng.as_ref()) {
                (Some(a), Some(b)) => Some(a.ts.min(b.ts)),
                (Some(a), None) => Some(a.ts),
                (None, Some(b)) => Some(b.ts),
                (None, None) => None,
            };
            let Some(ts) = next_ts else { break };
            while next_tick < ts {
                self.emit_tick(next_tick, &mut emit);
                next_tick += self.interval_ns;
            }
            if nb.as_ref().is_some_and(|q| q.ts == ts) {
                if let Some(q) = nb.take() {
                    self.latest_bn.insert(q.symbol, (q.bid, q.ask));
                }
                nb = binance.next_quote()?;
            } else if let Some(q) = ng.take() {
                self.latest_bg.insert(q.symbol, (q.bid, q.ask));
                ng = bitget.next_quote()?;
            }
        }
        Ok(())
    }

    fn emit_tick(&self, t: i64, emit: &mut impl FnMut(&str, Sample)) {
        for (sym, (gb, ga)) in &self.latest_bg {
            if let Some((nb, na)) = self.latest_bn.get(sym) {
                emit(
                    sym,
                    Sample {
                        ts: t,
                        bitget_bid: *gb,
                        bitget_ask: *ga,
                        binance_bid: *nb,
                        binance_ask: *na,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
