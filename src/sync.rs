//! 从研究桶同步运行目录：`aws s3 sync s3://<bucket>/<prefix> <data_dir>/<prefix>`，然后逐个子目录导入。
//!
//! 用 aws CLI 而不是 SDK：ledger 机器上已有实例角色和 CLI，依赖更少。

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Sync;
use crate::ingest::{self, Options, Outcome};
use crate::store::Store;

/// 一轮同步的结果。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Report {
    pub imported: Vec<String>,
    pub unchanged: usize,
    pub failed: Vec<(String, String)>,
}

fn local_dir(cfg: &Sync, prefix: &str) -> PathBuf {
    cfg.data_dir.join(prefix.trim_matches('/'))
}

/// 下载所有前缀。
pub fn download(cfg: &Sync) -> Result<()> {
    for prefix in &cfg.prefixes {
        let dest = local_dir(cfg, prefix);
        std::fs::create_dir_all(&dest).with_context(|| format!("建不了目录 {}", dest.display()))?;
        let src = format!("s3://{}/{}", cfg.bucket, prefix.trim_start_matches('/'));
        let mut cmd = Command::new(&cfg.aws);
        cmd.args(["s3", "sync", "--only-show-errors", "--no-progress"])
            .arg(&src)
            .arg(&dest);
        if !cfg.region.is_empty() {
            cmd.args(["--region", &cfg.region]);
        }
        let out = cmd
            .output()
            .with_context(|| format!("运行 {} 失败", cfg.aws))?;
        if !out.status.success() {
            bail!(
                "aws s3 sync {src} 失败：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(())
}

/// 导入本地数据目录下所有前缀里的运行目录。
pub fn import_all(store: &mut Store, cfg: &Sync, opt: &Options) -> Result<Report> {
    let mut rep = Report::default();
    for prefix in &cfg.prefixes {
        let root = local_dir(cfg, prefix);
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            import_one(store, &d, opt, &mut rep);
        }
    }
    Ok(rep)
}

fn import_one(store: &mut Store, d: &Path, opt: &Options, rep: &mut Report) {
    let name = d
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match ingest::import_dir(store, d, None, opt, ingest::now_ns()) {
        Ok(Outcome::Imported { run_id, .. }) => rep.imported.push(run_id),
        Ok(Outcome::Unchanged { .. }) => rep.unchanged += 1,
        Err(e) => rep.failed.push((name, format!("{e:#}"))),
    }
}
