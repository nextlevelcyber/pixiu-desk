//! 配置文件（TOML，示例见 `config/example.toml`）。
//!
//! 地址、桶名、路径只来自配置，代码里不写死。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::analysis::Params;
use crate::fx::Fx;
use crate::ingest::Options;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    #[serde(default)]
    pub prometheus: Prometheus,
    #[serde(default)]
    pub sync: Sync,
    #[serde(default)]
    pub analysis: Analysis,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub listen: String,
    pub db: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prometheus {
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

fn default_timeout() -> u64 {
    10
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub interval_s: u64,
    #[serde(default = "default_aws")]
    pub aws: String,
    #[serde(default)]
    pub region: String,
}

fn default_aws() -> String {
    "aws".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Analysis {
    #[serde(default = "default_sample_ms")]
    pub sample_ms: i64,
    #[serde(default = "default_curve_ms")]
    pub curve_ms: i64,
    #[serde(default = "default_tol")]
    pub markout_tol_s: String,
}

fn default_sample_ms() -> i64 {
    100
}

fn default_curve_ms() -> i64 {
    10_000
}

fn default_tol() -> String {
    "1.5".to_owned()
}

impl Default for Analysis {
    fn default() -> Analysis {
        Analysis {
            sample_ms: default_sample_ms(),
            curve_ms: default_curve_ms(),
            markout_tol_s: default_tol(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("读不了 {}", path.display()))?;
        Config::parse(&text).with_context(|| format!("配置 {} 无效", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text)?;
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<()> {
        let placeholder = |s: &str| s.contains('<') && s.contains('>');
        for (name, v) in [
            ("server.listen", self.server.listen.as_str()),
            ("prometheus.url", self.prometheus.url.as_str()),
            ("sync.bucket", self.sync.bucket.as_str()),
            ("sync.region", self.sync.region.as_str()),
        ] {
            if placeholder(v) {
                bail!("{name} 还是占位符：{v}");
            }
        }
        if self.analysis.sample_ms <= 0 || self.analysis.curve_ms <= 0 {
            bail!("analysis.sample_ms 和 curve_ms 必须为正");
        }
        let tol = Fx::parse(&self.analysis.markout_tol_s)?;
        if tol.signum() <= 0 {
            bail!("analysis.markout_tol_s 必须大于 0");
        }
        if !self.sync.bucket.is_empty() {
            if self.sync.prefixes.is_empty() {
                bail!("配置了 sync.bucket 就要给 sync.prefixes");
            }
            if self.sync.data_dir.as_os_str().is_empty() {
                bail!("配置了 sync.bucket 就要给 sync.data_dir");
            }
        }
        Ok(())
    }

    pub fn import_options(&self) -> Result<Options> {
        Ok(Options {
            params: Params {
                markout_tol_s: Fx::parse(&self.analysis.markout_tol_s)?,
                ..Params::default()
            },
            sample_ms: self.analysis.sample_ms,
            curve_ms: self.analysis.curve_ms,
            force: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_is_refused_until_placeholders_are_replaced() {
        let text = include_str!("../config/example.toml");
        let err = Config::parse(text).expect_err("占位符");
        assert!(format!("{err:#}").contains("占位符"), "{err:#}");
        let filled = text
            .replace("<DESK_PORT>", "8090")
            .replace("<PROMETHEUS_PORT>", "9090")
            .replace("<BUCKET>", "example-bucket")
            .replace("<REGION>", "ap-northeast-1");
        let c = Config::parse(&filled).expect("填好的示例");
        assert_eq!(c.analysis.sample_ms, 100);
        assert_eq!(c.sync.prefixes.len(), 2);
        assert_eq!(c.import_options().expect("选项").curve_ms, 10_000);
    }

    #[test]
    fn minimal_config_needs_only_server() {
        let c = Config::parse("[server]\nlisten = \"127.0.0.1:1\"\ndb = \"x.sqlite\"\n")
            .expect("最小配置");
        assert!(c.prometheus.url.is_empty());
        assert!(c.sync.bucket.is_empty());
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(Config::parse("[server]\nlisten=\"a\"\ndb=\"b\"\ntypo=1\n").is_err());
    }
}
