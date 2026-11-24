//! `pixiu-desk` 命令行入口。
//!
//! 子命令：
//! - `report --journal <f>... [--basis <SYMBOL>=<tsv>]... [--instruments SYM0,SYM1]`：输出一次运行的
//!   盈亏和 markout（JSON，结构同 `live_report.py --json` 的对应部分）。
//! - `serve --config <toml>`：启动网页和后台同步。
//! - `import --db <sqlite> <dir>... [--id <run-id>] [--force]`：把运行目录导入数据库。
//! - `sync --config <toml>`：从研究桶同步一次并导入（不启动网页）。
//! - `samples --binance <pxr>... --bitget <pxr>... --symbol <SYM> [--interval-ms 100]`：从录制生成
//!   双 venue 联合样本，输出 basis TSV（与 Pixiu 回测的 `basis-<SYMBOL>.tsv` 同一格式）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use pixiu_desk::analysis::Params;
use pixiu_desk::fx::Fx;
use pixiu_desk::report;

const USAGE: &str = "用法：
  pixiu-desk serve --config <desk.toml>
  pixiu-desk import --db <desk.sqlite> <run-dir>... [--id <run-id>] [--force]
  pixiu-desk sync --config <desk.toml>
  pixiu-desk report --journal <file>... [--basis <SYMBOL>=<basis.tsv>]... [--instruments SYM0,SYM1,...]
                    [--qty-scale 8] [--price-scale 8] [--markout-tol-s 1.5]
  pixiu-desk samples --binance <file.pxr[.gz]>... --bitget <file.pxr[.gz]>... --symbol <SYM>... [--interval-ms 100]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pixiu-desk：{e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let Some((cmd, rest)) = args.split_first() else {
        bail!("缺少子命令\n{USAGE}");
    };
    match cmd.as_str() {
        "report" => cmd_report(rest),
        "samples" => cmd_samples(rest),
        "import" => cmd_import(rest),
        "serve" => cmd_serve(rest),
        "sync" => cmd_sync(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => bail!("未知子命令 {other:?}\n{USAGE}"),
    }
}

fn cmd_report(args: &[String]) -> Result<()> {
    let mut journals: Vec<PathBuf> = Vec::new();
    let mut basis: Vec<(String, PathBuf)> = Vec::new();
    let mut instruments = None;
    let mut params = Params::default();
    let mut it = args.iter();
    let mut want = None::<String>;
    while let Some(a) = it.next() {
        if a.starts_with("--") {
            want = Some(a.clone());
            match a.as_str() {
                "--journal" | "--basis" => continue,
                "--instruments" | "--qty-scale" | "--price-scale" | "--markout-tol-s" => {
                    let Some(v) = it.next() else {
                        bail!("{a} 缺少参数")
                    };
                    match a.as_str() {
                        "--instruments" => {
                            let m: BTreeMap<i64, String> = v
                                .split(',')
                                .enumerate()
                                .map(|(i, s)| {
                                    (i64::try_from(i).unwrap_or(i64::MAX), s.trim().to_owned())
                                })
                                .collect();
                            instruments = Some(m);
                        }
                        "--qty-scale" => params.qty_scale = v.parse()?,
                        "--price-scale" => params.price_scale = v.parse()?,
                        _ => params.markout_tol_s = Fx::parse(v)?,
                    }
                    want = None;
                    continue;
                }
                _ => bail!("未知参数 {a}\n{USAGE}"),
            }
        }
        match want.as_deref() {
            Some("--journal") => journals.push(PathBuf::from(a)),
            Some("--basis") => {
                let Some((sym, path)) = a.split_once('=') else {
                    bail!("--basis 的格式是 <SYMBOL>=<file>：{a}")
                };
                basis.push((sym.to_owned(), PathBuf::from(path)));
            }
            _ => bail!("多余的参数 {a}\n{USAGE}"),
        }
    }
    if journals.is_empty() {
        bail!("至少要一个 --journal\n{USAGE}");
    }
    let inputs = report::Inputs {
        journals: journals.iter().map(PathBuf::as_path).collect(),
        basis: basis
            .iter()
            .map(|(s, p)| (s.clone(), p.as_path()))
            .collect(),
        instruments,
    };
    let v = report::build(&inputs, &params)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn cmd_samples(args: &[String]) -> Result<()> {
    use std::collections::BTreeSet;
    use std::io::Write;

    use pixiu_desk::pxr::{QuoteStream, Sampler, Venue};

    let mut binance: Vec<PathBuf> = Vec::new();
    let mut bitget: Vec<PathBuf> = Vec::new();
    let mut symbols: BTreeSet<String> = BTreeSet::new();
    let mut interval_ms: i64 = 100;
    let mut want = None::<&str>;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--binance" | "--bitget" | "--symbol" => want = Some(a.as_str()),
            "--interval-ms" => {
                let Some(v) = it.next() else {
                    bail!("--interval-ms 缺少参数")
                };
                interval_ms = v.parse()?;
                want = None;
            }
            _ => match want {
                Some("--binance") => binance.push(PathBuf::from(a)),
                Some("--bitget") => bitget.push(PathBuf::from(a)),
                Some("--symbol") => {
                    symbols.insert(a.clone());
                }
                _ => bail!("多余的参数 {a}\n{USAGE}"),
            },
        }
    }
    if binance.is_empty() || bitget.is_empty() || symbols.is_empty() || interval_ms <= 0 {
        bail!("需要 --binance、--bitget、--symbol 和正的 --interval-ms\n{USAGE}");
    }
    let open_all = |v: &[PathBuf]| -> Result<Vec<Box<dyn std::io::BufRead>>> {
        v.iter().map(|p| report::open(p)).collect()
    };
    let sb = QuoteStream::new(Venue::Binance, open_all(&binance)?, &symbols)?;
    let sg = QuoteStream::new(Venue::Bitget, open_all(&bitget)?, &symbols)?;
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    writeln!(
        w,
        "symbol\tts\tbitget_bid\tbitget_ask\tbinance_bid\tbinance_ask"
    )?;
    let mut err = None;
    Sampler::new(interval_ms * 1_000_000).run(sb, sg, |sym, s| {
        if err.is_none()
            && let Err(e) = writeln!(
                w,
                "{sym}\t{}\t{}\t{}\t{}\t{}",
                s.ts, s.bitget_bid, s.bitget_ask, s.binance_bid, s.binance_ask
            )
        {
            err = Some(e);
        }
    })?;
    if let Some(e) = err {
        return Err(e.into());
    }
    w.flush()?;
    Ok(())
}

/// `--config <file>`。
fn config_arg(args: &[String]) -> Result<pixiu_desk::config::Config> {
    match args {
        [flag, path] if flag == "--config" => pixiu_desk::config::Config::load(path.as_ref()),
        _ => bail!("需要 --config <desk.toml>\n{USAGE}"),
    }
}

fn cmd_serve(args: &[String]) -> Result<()> {
    let cfg = config_arg(args)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(pixiu_desk::web::serve(cfg))
}

fn cmd_sync(args: &[String]) -> Result<()> {
    let cfg = config_arg(args)?;
    if cfg.sync.bucket.is_empty() {
        bail!("配置里没有 sync.bucket");
    }
    pixiu_desk::sync::download(&cfg.sync)?;
    let mut store = pixiu_desk::store::Store::open(&cfg.server.db)?;
    let rep = pixiu_desk::sync::import_all(&mut store, &cfg.sync, &cfg.import_options()?)?;
    println!("{}", serde_json::to_string_pretty(&rep)?);
    if rep.failed.is_empty() {
        Ok(())
    } else {
        bail!("{} 个目录导入失败", rep.failed.len())
    }
}

fn cmd_import(args: &[String]) -> Result<()> {
    use pixiu_desk::ingest::{self, Options, Outcome};

    let mut db = None::<PathBuf>;
    let mut id = None::<String>;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut opt = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => db = it.next().map(PathBuf::from),
            "--id" => id = it.next().cloned(),
            "--force" => opt.force = true,
            "--sample-ms" => {
                opt.sample_ms = it.next().map(|v| v.parse()).transpose()?.unwrap_or(100);
            }
            _ if a.starts_with("--") => bail!("未知参数 {a}\n{USAGE}"),
            _ => dirs.push(PathBuf::from(a)),
        }
    }
    let Some(db) = db else {
        bail!("需要 --db\n{USAGE}")
    };
    if dirs.is_empty() {
        bail!("至少要一个运行目录\n{USAGE}");
    }
    if id.is_some() && dirs.len() > 1 {
        bail!("--id 只能配一个目录");
    }
    let mut store = pixiu_desk::store::Store::open(&db)?;
    let mut failed = 0;
    for d in &dirs {
        match ingest::import_dir(&mut store, d, id.as_deref(), &opt, ingest::now_ns()) {
            Ok(Outcome::Imported { run_id, fills }) => println!("导入 {run_id}：{fills} 笔成交"),
            Ok(Outcome::Unchanged { run_id }) => println!("跳过 {run_id}：内容未变"),
            Err(e) => {
                failed += 1;
                eprintln!("导入 {} 失败：{e:#}", d.display());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} 个目录导入失败")
    } else {
        Ok(())
    }
}
