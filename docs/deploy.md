# 部署（ledger 机器）

在任何 AWS 主机上的安装、启用、启动、改配置，都要所有者在当次会话里批准（AGENTS.md）。
下面的 `<...>` 是占位符，真实值只写在主机上。

## 机器

- `pixiu-ledger`：t4g.small（**ARM64**，2 vCPU，2 GiB），已运行 Prometheus（和待退役的 Grafana）。
- desk 与 Prometheus 同机，Prometheus 只监听 loopback，desk 也只监听 loopback。
- 资源：导入两小时的 `.pxr`（约 60 MB gzip）用时约 3 秒、内存峰值约 25 MB（x86 实测）；unit 里限制
  `MemoryMax=768M`、`CPUQuota=150%`。

## 构建（ARM64）

本仓库不安装或更换工具链。两种方式，选一种（都需要所有者同意）：

1. 在 ledger 机器上用 Rust 1.97 构建：`cargo build --release --locked`（需要在主机上装 Rust，属于主机变更）。
2. 在一台 ARM64 构建机（例如一次性的 Graviton 实例）上构建，把 `target/release/pixiu-desk` 拷到 ledger 机器。

二进制是自包含的（页面、ECharts、SQLite 都编译在里面），运行时只依赖 glibc 和 `aws` CLI（同步用）。

## 安装

```
sudo useradd --system --home /var/lib/pixiu-desk --shell /usr/sbin/nologin pixiu-desk
sudo install -m 0755 pixiu-desk /usr/local/bin/pixiu-desk
sudo install -d -m 0755 /etc/pixiu-desk
sudo install -m 0644 desk.toml /etc/pixiu-desk/desk.toml      # 由 config/example.toml 填好占位符
sudo install -m 0644 deploy/systemd/pixiu-desk.service /etc/systemd/system/pixiu-desk.service
sudo systemctl daemon-reload
sudo systemctl enable --now pixiu-desk
```

配置检查：`pixiu-desk sync --config /etc/pixiu-desk/desk.toml`（同步一次并打印导入结果）。

## 权限

同步用实例角色调用 `aws s3 sync`，角色需要研究桶上 `desk/` 前缀的 `s3:ListBucket` 和 `s3:GetObject`。
desk 不写桶。

## 访问

desk 只监听 `127.0.0.1:<DESK_PORT>`，不开入站端口。从自己的电脑做 SSM 端口转发：

```
aws ssm start-session --target <ledger 实例 ID> \
  --document-name AWS-StartPortForwardingSession \
  --parameters '{"portNumber":["<DESK_PORT>"],"localPortNumber":["8090"]}'
```

然后浏览器打开 `http://localhost:8090/`。

## 研究桶里的目录布局

每个子目录是一次运行（`ingest` 模块的文档列出可识别的文件）：

```
s3://<BUCKET>/desk/backtests/<run-id>/   summary.json、journal.jsonl、basis-<SYMBOL>.tsv、desk-meta.json
s3://<BUCKET>/desk/live/<run-id>/        journal-*.jsonl.gz、binance-md-*.pxr.gz、bitget-public-*.pxr.gz、desk-meta.json
```

`desk/live/` 的上传脚本属于第二期（交易主机上的变更，需要单独批准）；在此之前实盘运行可以手工放进去，
或在本地用 `pixiu-desk import` 导入。
