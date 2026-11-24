//! Web 冒烟测试：内存数据库导入黄金样本目录，起服务（端口 0），用 HTTP 访问页面和接口。

use std::path::Path;
use std::sync::{Arc, Mutex};

use pixiu_desk::config::Config;
use pixiu_desk::ingest::{self, Options};
use pixiu_desk::store::Store;
use pixiu_desk::web::{AppState, SyncStatus, router};
use serde_json::Value;

async fn start() -> String {
    let mut store = Store::open_in_memory().expect("数据库");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bt-btc-2h");
    ingest::import_dir(&mut store, &dir, Some("bt-1"), &Options::default(), 1).expect("导入");
    let cfg =
        Config::parse("[server]\nlisten = \"127.0.0.1:0\"\ndb = \":memory:\"\n").expect("配置");
    let st = Arc::new(AppState {
        store: Mutex::new(store),
        prom: None,
        opt: cfg.import_options().expect("选项"),
        cfg,
        sync: Mutex::new(SyncStatus::default()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("监听");
    let addr = listener.local_addr().expect("地址");
    tokio::spawn(async move { axum::serve(listener, router(st)).await.expect("服务") });
    format!("http://{addr}")
}

#[tokio::test]
async fn pages_and_run_api() {
    let base = start().await;
    let c = reqwest::Client::new();
    let page = c.get(format!("{base}/")).send().await.expect("请求");
    assert_eq!(page.status(), 200);
    assert!(page.text().await.expect("正文").contains("Pixiu Desk"));
    for p in ["/app.js", "/vendor/echarts.min.js"] {
        assert_eq!(
            c.get(format!("{base}{p}"))
                .send()
                .await
                .expect("请求")
                .status(),
            200,
            "{p}"
        );
    }

    let runs: Value = c
        .get(format!("{base}/api/runs"))
        .send()
        .await
        .expect("请求")
        .json()
        .await
        .expect("JSON");
    assert_eq!(runs[0]["run_id"], "bt-1");
    assert_eq!(runs[0]["fills"], 41);
    assert!(runs[0]["summary"].is_null(), "列表不带 summary 原文");

    let run: Value = c
        .get(format!("{base}/api/runs/bt-1"))
        .send()
        .await
        .expect("请求")
        .json()
        .await
        .expect("JSON");
    assert!(run["summary"].is_object());
    let fills: Value = c
        .get(format!("{base}/api/runs/bt-1/fills"))
        .send()
        .await
        .expect("请求")
        .json()
        .await
        .expect("JSON");
    assert_eq!(fills.as_array().map(Vec::len), Some(41));
    let curve: Value = c
        .get(format!("{base}/api/runs/bt-1/curve?symbol=BTCUSDT"))
        .send()
        .await
        .expect("请求")
        .json()
        .await
        .expect("JSON");
    assert!(curve.as_array().is_some_and(|a| a.len() > 700));

    let missing = c
        .get(format!("{base}/api/runs/nope"))
        .send()
        .await
        .expect("请求");
    assert_eq!(missing.status(), 404);
    let no_prom = c
        .get(format!("{base}/api/prom/query?query=up"))
        .send()
        .await
        .expect("请求");
    assert_eq!(no_prom.status(), 503, "未配置 Prometheus");
    let no_sync = c
        .post(format!("{base}/api/sync"))
        .send()
        .await
        .expect("请求");
    assert_eq!(no_sync.status(), 400, "未配置同步");
}
