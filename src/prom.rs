//! Prometheus HTTP API 的只读代理：`/api/v1/query`、`/api/v1/query_range`、`/api/v1/alerts`。
//!
//! 网页的实盘页通过 desk 访问 Prometheus（同源，不用 CORS），desk 只转发这三个只读端点。

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Prom {
    base: String,
    http: reqwest::Client,
}

/// 允许转发的端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Query,
    QueryRange,
    Alerts,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Endpoint::Query => "/api/v1/query",
            Endpoint::QueryRange => "/api/v1/query_range",
            Endpoint::Alerts => "/api/v1/alerts",
        }
    }

    /// 每个端点只接受的查询参数。
    fn allowed(self) -> &'static [&'static str] {
        match self {
            Endpoint::Query => &["query", "time"],
            Endpoint::QueryRange => &["query", "start", "end", "step"],
            Endpoint::Alerts => &[],
        }
    }
}

impl Prom {
    pub fn new(base: &str, timeout_s: u64) -> Result<Prom> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_s.max(1)))
            .build()
            .context("建 HTTP 客户端")?;
        Ok(Prom {
            base: base.trim_end_matches('/').to_owned(),
            http,
        })
    }

    /// 转发一次查询；返回 Prometheus 的 JSON 原文（`status`、`data` 或 `error`）。
    pub async fn get(&self, ep: Endpoint, params: &[(String, String)]) -> Result<Value> {
        let allowed = ep.allowed();
        let mut q: Vec<(&str, &str)> = Vec::new();
        for (k, v) in params {
            if !allowed.contains(&k.as_str()) {
                bail!("不允许的参数 {k}");
            }
            q.push((k, v));
        }
        let url = format!("{}{}", self.base, ep.path());
        let resp = self
            .http
            .get(&url)
            .query(&q)
            .send()
            .await
            .context("访问 Prometheus 失败")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("Prometheus 返回的不是 JSON")?;
        if !status.is_success() && body.get("status").is_none() {
            bail!("Prometheus 返回 HTTP {status}");
        }
        Ok(body)
    }
}
