//! `pixiu-desk` 命令行入口。
//!
//! 子命令：
//! - `report --journal <f>... [--basis <SYMBOL>=<tsv>]... [--instruments SYM0,SYM1]`：输出一次运行的
//!   盈亏和 markout（JSON，结构同 `live_report.py --json` 的对应部分）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use pixiu_desk::analysis::Params;
use pixiu_desk::fx::Fx;
use pixiu_desk::report;

const USAGE: &str = "用法：
  pixiu-desk report --journal <file>... [--basis <SYMBOL>=<basis.tsv>]... [--instruments SYM0,SYM1,...]
                    [--qty-scale 8] [--price-scale 8] [--markout-tol-s 1.5]";

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
