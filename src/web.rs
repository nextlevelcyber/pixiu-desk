//! HTTP 服务：静态页面（编译进二进制）、运行数据 API、Prometheus 只读代理、后台同步。
//!
//! 所有接口都是只读的；唯一的写操作是 `POST /api/sync`，它只触发 desk 自己的同步和导入。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::ingest::{Options, now_ns};
use crate::prom::{Endpoint, Prom};
use crate::store::Store;
use crate::sync;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const ECHARTS_JS: &[u8] = include_bytes!("../web/vendor/echarts.min.js");

/// 最近一次同步的状态（总览页显示）。
#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub running: bool,
    pub last_started_ns: Option<i64>,
    pub last_finished_ns: Option<i64>,
    pub last_error: Option<String>,
    pub last_report: Option<sync::Report>,
}

/// 服务的共享状态。
#[derive(Debug)]
pub struct AppState {
    pub store: Mutex<Store>,
    pub prom: Option<Prom>,
    pub cfg: Config,
    pub opt: Options,
    pub sync: Mutex<SyncStatus>,
}

type Shared = Arc<AppState>;

/// 接口错误：JSON `{"error": ...}`。
#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> ApiError {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult = std::result::Result<axum::Json<Value>, ApiError>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // 持锁的线程 panic 后数据仍然可用（存储操作都在事务里）。
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 在阻塞线程池里访问存储。
async fn with_store<T: Send + 'static>(
    st: &Shared,
    f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
) -> std::result::Result<T, ApiError> {
    let st = st.clone();
    tokio::task::spawn_blocking(move || f(&mut lock(&st.store)))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}

pub fn router(st: Shared) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/vendor/echarts.min.js", get(echarts_js))
        .route("/api/health", get(health))
        .route("/api/runs", get(runs))
        .route("/api/runs/{id}", get(run))
        .route("/api/runs/{id}/fills", get(fills))
        .route("/api/runs/{id}/curve", get(curve))
        .route("/api/prom/query", get(prom_query))
        .route("/api/prom/query_range", get(prom_query_range))
        .route("/api/prom/alerts", get(prom_alerts))
        .route("/api/sync", post(sync_now))
        .with_state(st)
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        INDEX_HTML,
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        APP_JS,
    )
}

async fn echarts_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "max-age=86400"),
        ],
        ECHARTS_JS,
    )
}

async fn health(State(st): State<Shared>) -> ApiResult {
    let runs = with_store(&st, |s| Ok(s.runs()?.len())).await?;
    let sync = lock(&st.sync).clone();
    Ok(axum::Json(json!({
        "ok": true,
        "now_ns": now_ns(),
        "runs": runs,
        "prometheus": st.prom.is_some(),
        "sync": sync,
        "analysis": {
            "sample_ms": st.opt.sample_ms,
            "curve_ms": st.opt.curve_ms,
            "markout_tol_s": st.opt.params.markout_tol_s.to_plain(),
        },
    })))
}

async fn runs(State(st): State<Shared>) -> ApiResult {
    let rows = with_store(&st, |s| s.runs()).await?;
    // 列表不带 summary 原文，减小体积。
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            r.summary = None;
            serde_json::to_value(r).unwrap_or(Value::Null)
        })
        .collect();
    Ok(axum::Json(Value::Array(rows)))
}

async fn run(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult {
    let row = with_store(&st, move |s| s.run(&id)).await?;
    match row {
        Some(r) => Ok(axum::Json(
            serde_json::to_value(r).map_err(anyhow::Error::from)?,
        )),
        None => Err(ApiError(StatusCode::NOT_FOUND, "没有这次运行".into())),
    }
}

async fn fills(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult {
    let rows = with_store(&st, move |s| s.fills(&id)).await?;
    Ok(axum::Json(
        serde_json::to_value(rows).map_err(anyhow::Error::from)?,
    ))
}

async fn curve(
    State(st): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let symbol = q
        .get("symbol")
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "缺少 symbol".into()))?;
    let rows = with_store(&st, move |s| s.curve(&id, &symbol)).await?;
    Ok(axum::Json(
        serde_json::to_value(rows).map_err(anyhow::Error::from)?,
    ))
}

async fn prom_call(st: &Shared, ep: Endpoint, q: HashMap<String, String>) -> ApiResult {
    let Some(p) = &st.prom else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "未配置 prometheus.url".into(),
        ));
    };
    let params: Vec<(String, String)> = q.into_iter().collect();
    match p.get(ep, &params).await {
        Ok(v) => Ok(axum::Json(v)),
        Err(e) => Err(ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
    }
}

async fn prom_query(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    prom_call(&st, Endpoint::Query, q).await
}

async fn prom_query_range(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    prom_call(&st, Endpoint::QueryRange, q).await
}

async fn prom_alerts(State(st): State<Shared>) -> ApiResult {
    prom_call(&st, Endpoint::Alerts, HashMap::new()).await
}

async fn sync_now(State(st): State<Shared>) -> ApiResult {
    if st.cfg.sync.bucket.is_empty() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "未配置 sync.bucket".into(),
        ));
    }
    let started = run_sync(st.clone()).await;
    Ok(axum::Json(json!({"started": started})))
}

/// 跑一轮同步；已有一轮在跑时返回 `false`。
pub async fn run_sync(st: Shared) -> bool {
    {
        let mut s = lock(&st.sync);
        if s.running {
            return false;
        }
        s.running = true;
        s.last_started_ns = Some(now_ns());
    }
    let st2 = st.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<sync::Report> {
        sync::download(&st2.cfg.sync)?;
        // 导入用单独的连接（WAL 下读写并行），导入期间网页照常可读。
        let mut store = Store::open(&st2.cfg.server.db)?;
        sync::import_all(&mut store, &st2.cfg.sync, &st2.opt)
    })
    .await;
    let mut s = lock(&st.sync);
    s.running = false;
    s.last_finished_ns = Some(now_ns());
    match result {
        Ok(Ok(rep)) => {
            for (dir, err) in &rep.failed {
                eprintln!("pixiu-desk：导入 {dir} 失败：{err}");
            }
            s.last_error = None;
            s.last_report = Some(rep);
        }
        Ok(Err(e)) => {
            eprintln!("pixiu-desk：同步失败：{e:#}");
            s.last_error = Some(format!("{e:#}"));
        }
        Err(e) => s.last_error = Some(e.to_string()),
    }
    true
}

/// 启动服务（直到收到 SIGTERM 或 Ctrl-C）。
pub async fn serve(cfg: Config) -> Result<()> {
    let store = Store::open(&cfg.server.db)?;
    let prom = if cfg.prometheus.url.is_empty() {
        None
    } else {
        Some(Prom::new(&cfg.prometheus.url, cfg.prometheus.timeout_s)?)
    };
    let opt = cfg.import_options()?;
    let st = Arc::new(AppState {
        store: Mutex::new(store),
        prom,
        sync: Mutex::new(SyncStatus {
            enabled: !cfg.sync.bucket.is_empty(),
            ..SyncStatus::default()
        }),
        opt,
        cfg: cfg.clone(),
    });
    if !cfg.sync.bucket.is_empty() {
        let st = st.clone();
        let every = cfg.sync.interval_s;
        tokio::spawn(async move {
            loop {
                run_sync(st.clone()).await;
                if every == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(every)).await;
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(&cfg.server.listen)
        .await
        .with_context(|| format!("监听 {} 失败", cfg.server.listen))?;
    eprintln!("pixiu-desk：监听 {}", cfg.server.listen);
    axum::serve(listener, router(st))
        .with_graceful_shutdown(shutdown())
        .await
        .context("HTTP 服务异常退出")
}

async fn shutdown() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        () = term => {},
    }
    eprintln!("pixiu-desk：收到停止信号");
}
