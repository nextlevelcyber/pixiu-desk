/* Pixiu Desk 前端：原生 JS 加 ECharts，不需要构建。
   页面：总览（#/）、实盘（#/live）、运行（#/runs）、运行详情（#/run/<id>）、对比（#/compare/<id>,<id>）。
   数据：/api/*（desk 存储里的运行）与 /api/prom/*（Prometheus 只读代理）。全部只读。 */
(function () {
  "use strict";

  const HORIZONS = [0, 2, 10, 60, 300, 900];
  const POLL_OVERVIEW_MS = 5000;
  const POLL_LIVE_MS = 30000;
  const RANGES = [["6h", 6], ["24h", 24], ["3d", 72], ["7d", 168]];

  const state = { charts: [], timer: null, route: null, prefs: loadPrefs() };

  /* ---------- DOM ---------- */
  const byId = id => document.getElementById(id);
  /* 追加子节点：可以传多个，数组会展开，null、undefined、false 跳过。 */
  function add(node) {
    for (let i = 1; i < arguments.length; i++) {
      const kid = arguments[i];
      if (kid === null || kid === undefined || kid === false) continue;
      if (Array.isArray(kid)) { kid.forEach(k => add(node, k)); continue; }
      node.appendChild(kid instanceof Node ? kid : document.createTextNode(String(kid)));
    }
  }
  function h(tag, props) {
    const node = document.createElement(tag);
    if (props) {
      Object.keys(props).forEach(k => {
        const v = props[k];
        if (v === null || v === undefined || v === false) return;
        if (k === "class") node.className = v;
        else if (k === "text") node.textContent = String(v);
        else if (k === "on") Object.keys(v).forEach(ev => node.addEventListener(ev, v[ev]));
        else if (k === "checked" || k === "disabled") node[k] = true;
        else node.setAttribute(k, v === true ? "" : String(v));
      });
    }
    for (let i = 2; i < arguments.length; i++) add(node, arguments[i]);
    return node;
  }
  function fill(node) {
    node.textContent = "";
    for (let i = 1; i < arguments.length; i++) add(node, arguments[i]);
    return node;
  }
  function sheet(title, sub) {
    const body = h("div", { class: "body" });
    const head = h("div", { class: "sec-head" }, h("h2", null, title), sub ? h("span", { class: "sub" }, sub) : null);
    const el = h("section", { class: "sheet section" }, head, body);
    el.head = head;
    el.body = body;
    return el;
  }
  function seg(options, current, onPick) {
    return h("span", { class: "seg", role: "group" }, options.map(o =>
      h("button", { type: "button", "aria-pressed": String(o[0] === current), on: { click: () => onPick(o[0]) } }, o[1])));
  }

  /* ---------- 偏好（只存本机，读不到也能用） ---------- */
  function loadPrefs() {
    try { return JSON.parse(localStorage.getItem("pixiu-desk-prefs") || "{}") || {}; } catch (e) { return {}; }
  }
  function savePrefs() {
    try { localStorage.setItem("pixiu-desk-prefs", JSON.stringify(state.prefs)); } catch (e) { /* 忽略 */ }
  }
  function currentTheme() {
    const t = document.documentElement.dataset.theme;
    if (t === "light" || t === "dark") return t;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  function setTheme(t) {
    document.documentElement.dataset.theme = t;
    try { localStorage.setItem("pixiu-desk-theme", t); } catch (e) { /* 忽略 */ }
    route();
  }

  /* ---------- 格式 ---------- */
  const pad = n => String(n).padStart(2, "0");
  function fmtTime(ns) {
    if (ns === null || ns === undefined) return "—";
    const d = new Date(ns / 1e6);
    return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) + " " + pad(d.getHours()) + ":" + pad(d.getMinutes()) + ":" + pad(d.getSeconds());
  }
  function fmtDur(ns) {
    if (!Number.isFinite(ns) || ns < 0) return "—";
    const s = Math.round(ns / 1e9);
    const hh = Math.floor(s / 3600), mm = Math.floor(s % 3600 / 60);
    return hh > 0 ? hh + " 小时 " + mm + " 分" : mm + " 分 " + (s % 60) + " 秒";
  }
  const isNum = v => v !== null && v !== undefined && v !== "" && v !== "-" && Number.isFinite(Number(v));
  function fmtNum(v, dp) {
    if (!isNum(v)) return "—";
    return Number(v).toLocaleString("en-US", { minimumFractionDigits: dp, maximumFractionDigits: dp });
  }
  function signed(v, dp, unit) {
    if (!isNum(v)) return h("span", null, "—");
    const n = Number(v);
    const text = (n > 0 ? "+" : n < 0 ? "−" : "") + fmtNum(Math.abs(n), dp) + (unit || "");
    return h("span", { class: n > 0 ? "up" : n < 0 ? "down" : "" }, text);
  }
  /* 金额：字符串原样显示精确值（截到 6 位小数），带正负色。 */
  function money(v) {
    if (!isNum(v)) return h("span", null, v === "-" ? "—" : (v || "—"));
    const n = Number(v);
    const dp = Math.abs(n) >= 100 ? 2 : Math.abs(n) >= 1 ? 4 : 6;
    return signed(v, dp);
  }
  /* 手续费：正值是成本、负值是返佣（按交易所原样），不着盈亏色，只显示数值。 */
  function fee(v) {
    if (!isNum(v)) return h("span", null, "—");
    const n = Number(v);
    return h("span", null, fmtNum(n, Math.abs(n) >= 100 ? 2 : Math.abs(n) >= 1 ? 4 : 6));
  }
  function css(name) { return getComputedStyle(document.body).getPropertyValue(name).trim(); }

  /* ---------- 接口 ---------- */
  async function api(path, init) {
    const r = await fetch(path, Object.assign({ cache: "no-store", headers: { Accept: "application/json" } }, init || {}));
    let body = null;
    try { body = await r.json(); } catch (e) { /* 非 JSON */ }
    if (!r.ok) throw new Error((body && body.error) || ("HTTP " + r.status));
    return body;
  }
  async function prom(query) {
    const j = await api("/api/prom/query?query=" + encodeURIComponent(query));
    if (j.status !== "success") throw new Error(j.error || "Prometheus 查询失败");
    return j.data.result;
  }
  async function promRange(query, startS, endS, stepS) {
    const q = "query=" + encodeURIComponent(query) + "&start=" + startS + "&end=" + endS + "&step=" + stepS;
    const j = await api("/api/prom/query_range?" + q);
    if (j.status !== "success") throw new Error(j.error || "Prometheus 查询失败");
    return j.data.result;
  }

  /* ---------- 图表 ---------- */
  function disposeCharts() { state.charts.forEach(c => c.dispose()); state.charts = []; }
  function chartIn(el) {
    const c = echarts.init(el, null, { renderer: "canvas" });
    state.charts.push(c);
    return c;
  }
  window.addEventListener("resize", () => state.charts.forEach(c => c.resize()));
  function palette() {
    return [css("--cobalt"), css("--amber"), css("--teal"), css("--violet"), css("--red"), css("--green"), css("--slate")];
  }
  function axisStyle() {
    return {
      axisLine: { lineStyle: { color: css("--rule") } },
      axisLabel: { color: css("--muted"), hideOverlap: true },
      splitLine: { lineStyle: { color: css("--grid") } },
    };
  }
  function baseOption(extra) {
    return Object.assign({
      color: palette(),
      textStyle: { fontFamily: css("--font"), color: css("--ink") },
      grid: { left: 64, right: 56, top: 48, bottom: 36 },
      tooltip: { trigger: "axis", backgroundColor: css("--sheet"), borderColor: css("--rule"), textStyle: { color: css("--ink") } },
      legend: { top: 2, textStyle: { color: css("--muted") }, itemWidth: 14, itemHeight: 8 },
    }, extra);
  }
  function timeAxis() { return Object.assign({ type: "time" }, axisStyle()); }
  function valueAxis(name, extra) {
    return Object.assign({ type: "value", name: name || "", nameTextStyle: { color: css("--muted") }, scale: true }, axisStyle(), extra || {});
  }
  function line(name, data, extra) {
    return Object.assign({ name, type: "line", data, showSymbol: false, lineStyle: { width: 1.5 } }, extra || {});
  }
  function placeholder(el, text) { fill(el, h("div", { class: "empty" }, text)); }

  /* ---------- 顶栏 ---------- */
  const PAGES = [["", "总览"], ["live", "实盘"], ["runs", "运行"]];
  function renderTop(active, health) {
    const theme = currentTheme();
    const themes = h("span", { class: "theme", role: "group", "aria-label": "主题" },
      [["light", "亮"], ["dark", "暗"]].map(x => h("button", { type: "button", "aria-pressed": String(x[0] === theme), on: { click: () => setTheme(x[0]) } }, x[1])));
    let stat = null;
    if (health) {
      const s = health.sync || {};
      const tone = !s.enabled ? "warn" : s.last_error ? "bad" : "ok";
      const text = !s.enabled ? "未配置同步" : s.running ? "同步中" : s.last_error ? "同步失败" : s.last_finished_ns ? "已同步 " + fmtTime(s.last_finished_ns) : "等待同步";
      stat = h("span", { class: "stat " + tone }, "运行 " + health.runs + " 次", h("b", null, text));
    }
    fill(byId("top"),
      h("span", { class: "brand" }, "Pixiu Desk", h("small", null, "个人只读后台")),
      h("nav", { class: "nav" }, PAGES.map(p => h("a", { href: "#/" + p[0], "aria-current": p[0] === active ? "page" : null }, p[1]))),
      h("span", { class: "spacer" }), stat, themes);
  }

  /* ---------- 路由 ---------- */
  function parseRoute() {
    const raw = (location.hash.replace(/^#\/?/, "") || "");
    const [path, query] = raw.split("?");
    const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
    const params = new URLSearchParams(query || "");
    if (parts[0] === "live") return { page: "live", params };
    if (parts[0] === "runs") return { page: "runs", params };
    if (parts[0] === "run" && parts[1]) return { page: "run", id: parts[1], params };
    if (parts[0] === "compare" && parts[1]) return { page: "compare", ids: parts[1].split(",").filter(Boolean), params };
    return { page: "overview", params };
  }
  function stopTimer() { if (state.timer) { clearTimeout(state.timer); state.timer = null; } }
  function poll(fn, ms) {
    stopTimer();
    const token = {};
    state.pollToken = token;
    const tick = async () => {
      if (state.pollToken !== token) return;
      if (!document.hidden) { try { await fn(); } catch (e) { /* 各区块自己显示错误 */ } }
      if (state.pollToken === token) state.timer = setTimeout(tick, ms);
    };
    tick();
  }
  document.addEventListener("visibilitychange", () => { if (!document.hidden && state.route) route(); });

  async function route() {
    stopTimer();
    state.pollToken = null;
    disposeCharts();
    const r = parseRoute();
    state.route = r;
    const active = r.page === "overview" ? "" : r.page === "live" ? "live" : "runs";
    renderTop(active, null);
    api("/api/health").then(hl => { if (state.route === r) renderTop(active, hl); }).catch(() => {});
    const main = byId("main");
    fill(main);
    try {
      if (r.page === "overview") await pageOverview(main);
      else if (r.page === "live") await pageLive(main, r);
      else if (r.page === "runs") await pageRuns(main, r);
      else if (r.page === "run") await pageRun(main, r);
      else if (r.page === "compare") await pageCompare(main, r);
    } catch (e) {
      fill(main, h("div", { class: "banner bad" }, "页面加载失败：" + e.message));
    }
  }
  window.addEventListener("hashchange", route);

  /* ---------- 共用：运行的摘要数字 ---------- */
  function runTotals(run) {
    const pnl = run.pnl || {};
    let total = 0, fees = 0, any = false;
    Object.values(pnl).forEach(p => {
      if (isNum(p.total)) { total += Number(p.total); any = true; }
      if (isNum(p.fees)) fees += Number(p.fees);
    });
    return { total: any ? total : null, fees };
  }
  /* 合约 symbol 在 horizon h、参考 ref 下的 markout 均值（bps）。 */
  function markoutMean(run, symbol, hz, ref) {
    const m = run.markout && run.markout[symbol];
    const c = m && m.aggregate && m.aggregate[String(hz)] && m.aggregate[String(hz)][ref];
    return c ? c.mean_bps : null;
  }
  function firstSymbol(run) {
    const s = Object.keys(run.pnl || {});
    return s.length ? s[0] : null;
  }
  function runName(run) { return run.label || run.run_id; }
  function sourceChip(src) { return h("span", { class: "chip " + src }, src === "backtest" ? "回测" : "实盘"); }

  /* ================= 总览 ================= */
  async function pageOverview(main) {
    const proc = sheet("进程", "来自 Prometheus，5 秒刷新");
    const alerts = sheet("告警", "Prometheus 告警规则");
    const recent = sheet("最近运行", "desk 存储里的实盘和回测");
    add(main, proc, alerts, recent);
    poll(async () => {
      await Promise.all([overviewProcess(proc.body), overviewAlerts(alerts.body)]);
    }, POLL_OVERVIEW_MS);
    await overviewRecent(recent.body);
  }

  async function overviewProcess(el) {
    let res;
    try {
      res = await Promise.all([
        prom("max(pixiu_up)"),
        prom("pixiu_build_info"),
        prom("max by (mode) (pixiu_mode) == 1"),
        prom("max by (reason) (pixiu_hold) == 1"),
        prom("max(pixiu_halt)"),
        prom("pixiu_position"),
        prom("sum by (instrument) (pixiu_live_orders)"),
        prom("pixiu:session_pnl:usdt"),
        prom("max by (instrument) (pixiu_mark_price)"),
        prom("pixiu:fees:usdt"),
        prom("max by (instrument) (pixiu_feed_health)"),
        prom("pixiu:equity_change:usdt"),
      ]);
    } catch (e) {
      placeholder(el, "读不到 Prometheus：" + e.message);
      return;
    }
    const [up, info, mode, hold, halt, pos, orders, spnl, mark, fees, feed, eq] = res;
    const one = r => (r && r.length ? Number(r[0].value[1]) : null);
    if (!up.length) {
      fill(el, h("div", { class: "empty" }, "没有 pixiu 的指标：进程未运行，或交易主机已停机（指标 5 分钟后过期）。实盘历史见「实盘」页。"));
      return;
    }
    const bi = info.length ? info[0].metric : {};
    const holds = hold.map(x => x.metric.reason).filter(Boolean);
    const kpis = h("div", { class: "kpis" },
      h("span", { class: "kpi big" }, h("span", { class: "lbl" }, "状态"), h("b", { class: one(up) === 1 ? "up" : "down" }, one(up) === 1 ? "运行中" : "未运行")),
      h("span", { class: "kpi" }, h("span", { class: "lbl" }, "模式"), h("b", null, mode.map(x => x.metric.mode).join("、") || "—")),
      h("span", { class: "kpi" }, h("span", { class: "lbl" }, "hold"), h("b", { class: holds.length ? "down" : "" }, holds.length ? holds.join("、") : "无")),
      h("span", { class: "kpi" }, h("span", { class: "lbl" }, "halt"), h("b", { class: one(halt) === 1 ? "down" : "" }, one(halt) === 1 ? "已触发" : "否")),
      h("span", { class: "kpi" }, h("span", { class: "lbl" }, "权益变化（USDT）"), h("b", null, money(one(eq)))),
      h("span", { class: "kpi" }, h("span", { class: "lbl" }, "版本"), h("b", null, (bi.version || "—") + " · " + (bi.mode || "") + " · 配置 " + (bi.config_hash || "—"))),
      bi.run ? h("span", { class: "kpi" }, h("span", { class: "lbl" }, "启动"), h("b", null, fmtTime(Number(bi.run) * 1e6))) : null);
    const by = (rows, key) => { const m = {}; rows.forEach(r => { m[r.metric.instrument] = Number(r.value[1]); }); return m; };
    const P = by(pos, "position"), O = by(orders), S = by(spnl), M = by(mark), F = by(fees), H = by(feed);
    const instruments = Array.from(new Set([].concat(Object.keys(P), Object.keys(S), Object.keys(M)))).sort();
    const table = h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, ["合约", "feed", "持仓", "在场单", "mark", "会话盈亏（USDT）", "手续费（USDT）"].map((t, i) => h("th", { class: i >= 2 ? "r" : null }, t)))),
      h("tbody", null, instruments.map(i => h("tr", null,
        h("td", null, h("b", null, i)),
        h("td", null, H[i] === 0 ? h("span", { class: "chip ok" }, "正常") : H[i] === 1 ? h("span", { class: "chip firing" }, "已撤出") : "—"),
        h("td", { class: "r" }, fmtNum(P[i], 4)),
        h("td", { class: "r" }, fmtNum(O[i], 0)),
        h("td", { class: "r" }, fmtNum(M[i], 2)),
        h("td", { class: "r" }, money(S[i])),
        h("td", { class: "r" }, fee(F[i])))))));
    fill(el, kpis, instruments.length ? table : null);
  }

  async function overviewAlerts(el) {
    let j;
    try { j = await api("/api/prom/alerts"); } catch (e) { placeholder(el, "读不到告警：" + e.message); return; }
    const list = (j.data && j.data.alerts) || [];
    if (!list.length) { fill(el, h("div", { class: "empty" }, "没有触发中的告警。")); return; }
    list.sort((a, b) => (a.state === "firing" ? 0 : 1) - (b.state === "firing" ? 0 : 1));
    fill(el, h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, ["状态", "告警", "标签", "起始", "说明"].map(t => h("th", null, t)))),
      h("tbody", null, list.map(a => h("tr", null,
        h("td", null, h("span", { class: "chip " + (a.state === "firing" ? "firing" : "") }, a.state === "firing" ? "触发" : "待定")),
        h("td", null, h("b", null, a.labels.alertname || "—")),
        h("td", { class: "wrap" }, Object.keys(a.labels).filter(k => k !== "alertname").map(k => k + "=" + a.labels[k]).join(" ")),
        h("td", null, a.activeAt ? fmtTime(Date.parse(a.activeAt) * 1e6) : "—"),
        h("td", { class: "wrap" }, (a.annotations && (a.annotations.summary || a.annotations.description)) || "")))))));
  }

  async function overviewRecent(el) {
    const runs = await api("/api/runs");
    if (!runs.length) { fill(el, h("div", { class: "empty" }, "存储里还没有运行。用 pixiu-desk import 导入，或配置研究桶同步。")); return; }
    fill(el, runsTable(runs.slice(0, 10), null), h("div", { class: "note" }, h("a", { href: "#/runs" }, "全部运行 →")));
  }

  /* ================= 实盘 ================= */
  async function pageLive(main, r) {
    const rangeKey = r.params.get("range") || state.prefs.liveRange || "24h";
    const hours = (RANGES.find(x => x[0] === rangeKey) || RANGES[1])[1];
    const bar = h("div", { class: "sheet section" }, h("div", { class: "sec-head" }, h("h2", null, "实盘历史"),
      h("span", { class: "sub" }, "Prometheus 录制规则，" + POLL_LIVE_MS / 1000 + " 秒刷新；交易主机停机时显示最后一次运行"),
      h("span", { class: "spacer" }),
      seg(RANGES.map(x => [x[0], x[0]]), rangeKey, k => { state.prefs.liveRange = k; savePrefs(); location.hash = "#/live?range=" + k; })));
    const panels = [
      { title: "会话盈亏与带入库存（USDT）", q: [["会话 ", "pixiu:session_pnl:usdt"], ["带入 ", "pixiu:carried_inventory_pnl:usdt"]], tall: true,
        tip: "会话盈亏 = 本次运行成交的现金流 + 净成交量按 mark 计价 − USDT 手续费；带入库存 = 启动时持仓 × mark 变化。" },
      { title: "权益变化与体系外部分（USDT）", q: [["权益变化", "pixiu:equity_change:usdt"], ["体系外", "pixiu:outside_pnl:usdt"]],
        tip: "权益变化 = USDT 权益 − 启动锚点；体系外 = 权益变化 − 会话盈亏 − 带入库存（资金费、手工成交、划转）。" },
      { title: "持仓（基础币）", q: [["持仓 ", "pixiu_position"]], step: true },
      { title: "mark 价格", q: [["mark ", "max by (instrument) (pixiu_mark_price)"]] },
      { title: "每小时成交", q: [["", "sum by (instrument, side) (increase(pixiu_fills_total[1h]))"]] },
      { title: "手续费（USDT）", q: [["手续费 ", "pixiu:fees:usdt"]] },
      { title: "venue 溢价（bps）", q: [["溢价 ", "pixiu_basis_bps"]] },
    ];
    const grid = h("div", { class: "grid2" });
    const els = panels.map((p, i) => {
      const box = h("div", { class: "chart" + (p.tall ? " tall" : "") });
      const wrap = h("div", null, h("div", { class: "sec-head" }, h("h2", null, p.title)), box, p.tip ? h("div", { class: "note" }, p.tip) : null);
      if (i < 2) add(main, i === 0 ? bar : null, h("div", { class: "sheet section" }, wrap));
      else add(grid, wrap);
      return { p, box };
    });
    add(main, h("div", { class: "sheet section" }, grid));
    const charts = els.map(e => ({ p: e.p, box: e.box, chart: null }));
    poll(async () => {
      const end = Math.floor(Date.now() / 1000);
      const start = end - hours * 3600;
      const step = Math.max(15, Math.round(hours * 3600 / 720));
      await Promise.all(charts.map(async c => {
        let series = [];
        try {
          for (const [prefix, q] of c.p.q) {
            const res = await promRange(q, start, end, step);
            res.forEach(s => {
              const lbl = Object.keys(s.metric).filter(k => k !== "__name__").map(k => s.metric[k]).join(" ");
              series.push(line((prefix.trim() + " " + lbl).trim() || q, s.values.map(v => [v[0] * 1000, Number(v[1])]), c.p.step ? { step: "end" } : {}));
            });
          }
        } catch (e) {
          if (!c.chart) placeholder(c.box, "查询失败：" + e.message);
          return;
        }
        if (!series.length) { if (c.chart) { c.chart.dispose(); state.charts = state.charts.filter(x => x !== c.chart); c.chart = null; } placeholder(c.box, "这段时间没有数据。"); return; }
        if (!c.chart) { fill(c.box); c.chart = chartIn(c.box); }
        c.chart.setOption(baseOption({ xAxis: timeAxis(), yAxis: valueAxis(), series }), true);
      }));
    }, POLL_LIVE_MS);
  }

  /* ================= 运行列表 ================= */
  function runsTable(runs, selected) {
    const syms = run => Object.keys(run.pnl || {});
    return h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, [selected ? "" : null, "类型", "名称", "开始", "时长", "合约", "成交", "盈亏（计价币）", "手续费", "markout 2s", "markout 60s", "markout 300s"]
        .filter(t => t !== null).map((t, i) => h("th", { class: i >= (selected ? 6 : 5) ? "r" : null }, t)))),
      h("tbody", null, runs.map(run => {
        const t = runTotals(run);
        const s = firstSymbol(run);
        const go = () => { location.hash = "#/run/" + encodeURIComponent(run.run_id); };
        const box = selected ? h("input", { type: "checkbox", checked: selected.has(run.run_id), on: {
          click: e => { e.stopPropagation(); if (e.target.checked) selected.add(run.run_id); else selected.delete(run.run_id); if (selected.onChange) selected.onChange(); } } }) : null;
        return h("tr", { class: "item", tabindex: 0, on: { click: go, keydown: e => { if (e.key === "Enter") go(); } } },
          selected ? h("td", null, box) : null,
          h("td", null, sourceChip(run.source)),
          h("td", null, h("b", null, runName(run)), run.label ? h("span", { class: "sub" }, " " + run.run_id) : null),
          h("td", null, fmtTime(run.started_ns)),
          h("td", null, fmtDur(run.stopped_ns - run.started_ns)),
          h("td", null, syms(run).join("、") || "—"),
          h("td", { class: "r" }, run.fills),
          h("td", { class: "r" }, money(t.total)),
          h("td", { class: "r" }, fee(t.fees)),
          [2, 60, 300].map(hz => h("td", { class: "r" }, s ? signed(markoutMean(run, s, hz, "bitget"), 2) : "—")));
      }))));
  }

  async function pageRuns(main, r) {
    const filter = r.params.get("source") || "all";
    const runs = await api("/api/runs");
    const shown = runs.filter(x => filter === "all" || x.source === filter);
    const selected = new Set();
    const cmp = h("button", { class: "btn", type: "button", disabled: true, on: { click: () => {
      location.hash = "#/compare/" + Array.from(selected).map(encodeURIComponent).join(","); } } }, "对比所选");
    selected.onChange = () => { cmp.disabled = selected.size < 2; };
    const s = sheet("运行", "markout 为 Bitget 中间价参考的均值（bps），正值对成交有利");
    add(s.head, h("span", { class: "spacer" }),
      seg([["all", "全部"], ["live", "实盘"], ["backtest", "回测"]], filter, k => { location.hash = "#/runs?source=" + k; }), cmp);
    add(s.body, shown.length ? runsTable(shown, selected) : h("div", { class: "empty" }, "没有符合条件的运行。"));
    add(main, s);
  }

  /* ================= 运行详情 ================= */
  async function pageRun(main, r) {
    const [run, fills] = await Promise.all([
      api("/api/runs/" + encodeURIComponent(r.id)),
      api("/api/runs/" + encodeURIComponent(r.id) + "/fills"),
    ]);
    const symbols = Object.keys(run.pnl || {});
    const symbol = r.params.get("symbol") && symbols.includes(r.params.get("symbol")) ? r.params.get("symbol") : symbols[0];
    const ref = state.prefs.ref || "bitget";

    const head = sheet(runName(run), run.run_id);
    add(head.head, h("span", { class: "spacer" }), sourceChip(run.source));
    add(head.body, h("div", { class: "kpis" },
      kv("开始", fmtTime(run.started_ns)), kv("时长", fmtDur(run.stopped_ns - run.started_ns)),
      kv("版本", run.version || "—"), kv("配置", run.config_hash || "—"), kv("模式", run.mode || "—"),
      kv("markout 样本", run.sample_ms ? run.sample_ms + " ms" : "无"), kv("导入", fmtTime(run.imported_ns))));
    if (symbols.length > 1) add(head.body, h("div", { class: "tools" }, seg(symbols.map(x => [x, x]), symbol,
      k => { location.hash = "#/run/" + encodeURIComponent(run.run_id) + "?symbol=" + encodeURIComponent(k); })));
    add(main, head);
    if (!symbol) { add(main, h("div", { class: "sheet section empty" }, "这次运行没有成交。")); return; }

    const p = run.pnl[symbol];
    const mine = fills.filter(f => f.symbol === symbol);
    const pnl = sheet("盈亏 · " + symbol, "FIFO；未平部分按 " + (p.mark_source || "mark") + " 计价；total = 已实现 + 未实现 − 手续费");
    add(pnl.body, h("div", { class: "kpis" },
      kvb("合计", money(p.total)), kv("已实现", money(p.realised)), kv("未实现", money(p.unrealised)), kv("手续费", fee(p.fees)),
      kv("未平数量", p.open_qty), kv("未平均价", p.open_avg_cost), kv("mark", p.mark),
      kv("成交", mine.length + " 笔（买 " + mine.filter(f => f.side === "buy").length + "，卖 " + mine.filter(f => f.side === "sell").length + "）")));
    const curveBox = h("div", { class: "chart tall" });
    const priceBox = h("div", { class: "chart short" });
    add(pnl.body, curveBox, priceBox);
    add(main, pnl);

    const bd = run.breakdown && run.breakdown[symbol];
    const wf = sheet("收益拆分 · " + symbol, "参考 Bitget 中间价；合计与上面的 FIFO total 精确相等");
    const wfBox = h("div", { class: "chart" });
    const outside = h("div", { class: "note" });
    if (bd) {
      add(wf.body, h("div", { class: "kpis" },
        kv("价差收益", money(bd.edge)), kv("逆向选择（0–" + bd.horizon_s + " 秒）", money(bd.adverse)),
        kv("库存漂移（" + bd.horizon_s + " 秒后）", money(bd.drift)), kv("手续费", fee(bd.fees)), kvb("合计", money(bd.total))),
        wfBox,
        h("div", { class: "note" },
          "价差收益 = Σ 成交时（0 秒）中间价与成交价之差 × 数量；逆向选择 = Σ(" + bd.horizon_s + " 秒 markout − 0 秒 markout)；",
          "库存漂移 = 交易盈亏 − Σ " + bd.horizon_s + " 秒 markout（之后剩余持仓随价格的变动）。",
          bd.missing_edge || bd.missing_horizon ? "缺样本的成交：0 秒 " + bd.missing_edge + " 笔、" + bd.horizon_s + " 秒 " + bd.missing_horizon + " 笔，按 0 计入，差额落在库存漂移里。" : ""),
        run.source === "live" ? outside : null);
    } else {
      add(wf.body, h("div", { class: "empty" }, "没有拆分：这次运行缺样本（或导入时 desk 版本较旧，用 import --force 重新导入）。"));
    }
    add(main, wf);

    const mo = sheet("markout · " + symbol, "sign × (参考中间价 − 成交价) / 成交价 × 10000；正值表示市场朝有利于这笔成交的方向走");
    add(mo.head, h("span", { class: "spacer" }), seg([["bitget", "Bitget 中间价"], ["binance", "Binance 中间价"]], ref, k => { state.prefs.ref = k; savePrefs(); route(); }));
    const moBox = h("div", { class: "chart" });
    const hzPick = Number(state.prefs.hz || 60);
    const scatterBox = h("div", { class: "chart" });
    const m = run.markout && run.markout[symbol];
    add(mo.body, m ? [h("div", { class: "grid2" },
      h("div", null, moBox),
      h("div", null, h("div", { class: "tools" }, h("span", { class: "sub" }, "逐笔，horizon"),
        seg(HORIZONS.map(x => [x, x + "s"]), hzPick, k => { state.prefs.hz = k; savePrefs(); route(); })), scatterBox)),
      markoutTable(m, ref), levelTable(m, ref),
      h("div", { class: "note" }, "容差 " + m.tolerance_s + " 秒；缺样本的（成交, horizon）对 " + m.missing + " 个；报价时长 " + (m.quoting_span_s || "—") + " 秒。")]
      : h("div", { class: "empty" }, "没有样本，算不了 markout（运行目录里缺 basis TSV 或 .pxr 录制）。"));
    add(main, mo);

    const ft = sheet("成交 · " + symbol, "每笔在各 horizon 的 markout（bps，" + (ref === "bitget" ? "Bitget" : "Binance") + " 中间价）");
    add(ft.body, fillsTable(mine, ref));
    add(main, ft);

    const ck = sheet("数据核对", "输入是否完整、彼此是否对得上");
    add(ck.body, run.checks ? checksTable(run.checks) : h("div", { class: "empty" }, "没有核对结果（用 import --force 重新导入）。"));
    add(main, ck);

    if (run.summary) {
      const s = run.summary;
      const bt = sheet("回测参数", (s.fill_model && s.fill_model.used ? "成交模型 " + s.fill_model.used : "") + (s.tape_time ? " · 磁带时间 " + s.tape_time : ""));
      add(bt.body, s.model ? h("div", { class: "note" }, String(s.model)) : null,
        h("details", null, h("summary", null, "summary.json"), h("pre", { class: "json" }, JSON.stringify(s, null, 2))));
      add(main, bt);
    }
    if (run.meta) add(main, h("section", { class: "sheet section" }, h("details", null, h("summary", null, "desk-meta.json"), h("pre", { class: "json" }, JSON.stringify(run.meta, null, 2)))));

    // 图表
    const curve = await api("/api/runs/" + encodeURIComponent(run.run_id) + "/curve?symbol=" + encodeURIComponent(symbol));
    drawCurve(curveBox, curve);
    drawPrice(priceBox, curve, mine);
    if (m) { drawMarkout(moBox, [{ name: "", m }], ref); drawScatter(scatterBox, mine, hzPick, ref); }
    if (bd) drawWaterfall(wfBox, bd);
    if (bd && run.source === "live") outsideNote(outside, run);
  }
  function kv(label, value) { return h("span", { class: "kpi" }, h("span", { class: "lbl" }, label), h("b", null, value)); }
  function kvb(label, value) { return h("span", { class: "kpi big" }, h("span", { class: "lbl" }, label), h("b", null, value)); }

  /* 实盘运行：Prometheus 里这次运行结束时的「体系外」部分（资金费、手工成交、划转），不计入上面的合计。 */
  async function outsideNote(el, run) {
    const t = Math.floor((run.stopped_ns || 0) / 1e9);
    try {
      const j = await api("/api/prom/query?query=" + encodeURIComponent("last_over_time(pixiu:outside_pnl:usdt[10m])") + "&time=" + t);
      const r = j.data && j.data.result;
      if (!r || !r.length) { fill(el, "资金费和体系外：Prometheus 里没有这次运行结束时的权益线。"); return; }
      fill(el, "资金费和体系外（权益线，运行结束时，不计入上面的合计）：", money(r[0].value[1]), " USDT");
    } catch (e) {
      fill(el, "资金费和体系外：读不到 Prometheus（" + e.message + "）。");
    }
  }
  function drawWaterfall(el, bd) {
    const parts = [["价差收益", Number(bd.edge)], ["逆向选择", Number(bd.adverse)], ["库存漂移", Number(bd.drift)]];
    if (isNum(bd.fees)) parts.push(["手续费", -Number(bd.fees)]);
    const base = [], up = [], down = [];
    let acc = 0;
    parts.forEach(([, v]) => {
      const next = acc + v;
      base.push(Math.min(acc, next));
      up.push(v >= 0 ? v : null);
      down.push(v < 0 ? -v : null);
      acc = next;
    });
    const total = Number(bd.total);
    base.push(Math.min(0, total));
    up.push(total >= 0 ? total : null);
    down.push(total < 0 ? -total : null);
    const labels = parts.map(p => p[0]).concat(["合计"]);
    const values = parts.map(p => p[1]).concat([total]);
    const c = chartIn(el);
    c.setOption(baseOption({
      legend: { show: false },
      grid: { left: 64, right: 24, top: 24, bottom: 36 },
      tooltip: { trigger: "axis", axisPointer: { type: "shadow" }, backgroundColor: css("--sheet"), borderColor: css("--rule"), textStyle: { color: css("--ink") },
        formatter: ps => { const i = ps[0].dataIndex; return labels[i] + "：" + (values[i] > 0 ? "+" : "") + values[i].toFixed(6); } },
      xAxis: Object.assign({ type: "category", data: labels }, axisStyle()),
      yAxis: valueAxis("", { scale: false }),
      series: [
        { type: "bar", stack: "w", data: base, itemStyle: { color: "transparent" }, emphasis: { disabled: true } },
        { name: "增加", type: "bar", stack: "w", data: up, barMaxWidth: 64, itemStyle: { color: css("--green") } },
        { name: "减少", type: "bar", stack: "w", data: down, barMaxWidth: 64, itemStyle: { color: css("--red") } },
      ],
    }));
  }
  function checksTable(groups) {
    const chip = v => v === "ok" ? h("span", { class: "chip ok" }, "通过") : v === "warn" ? h("span", { class: "chip warn" }, "注意") : h("span", { class: "chip" }, "无从判断");
    return h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, ["", "项目", "数值", "说明"].map(t => h("th", null, t)))),
      h("tbody", null, groups.map(g => [
        h("tr", null, h("td", { colspan: 4, class: "grp" }, g.group)),
        g.items.map(i => h("tr", null, h("td", null, chip(i.verdict)), h("td", null, i.name), h("td", { class: "wrap" }, i.value), h("td", { class: "wrap sub" }, i.note))),
      ]))));
  }
  function markoutTable(m, ref) {
    const agg = m.aggregate || {};
    return h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, ["horizon", "样本", "均值 bps", "中位数 bps", "计价币合计", "正值占比"].map((t, i) => h("th", { class: i ? "r" : null }, t)))),
      h("tbody", null, HORIZONS.map(hz => {
        const c = (agg[String(hz)] || {})[ref] || {};
        return h("tr", null, h("td", null, hz + " 秒"), h("td", { class: "r" }, c.count),
          h("td", { class: "r" }, signed(c.mean_bps, 3)), h("td", { class: "r" }, signed(c.median_bps, 3)),
          h("td", { class: "r" }, money(c.quote_sum)), h("td", { class: "r" }, c.positive_share || "—"));
      }))));
  }
  function levelTable(m, ref) {
    const levels = m.by_level || [];
    if (levels.length < 1) return null;
    return h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, ["level", "成交", "每小时"].concat(HORIZONS.map(x => x + "s 均值")).map((t, i) => h("th", { class: i ? "r" : null }, t)))),
      h("tbody", null, levels.map(l => h("tr", null,
        h("td", null, l.level === null ? "未知" : "L" + l.level), h("td", { class: "r" }, l.fills), h("td", { class: "r" }, l.fills_per_hour || "—"),
        HORIZONS.map(hz => h("td", { class: "r" }, signed(((l.horizons[String(hz)] || {})[ref] || {}).mean_bps, 2))))))));
  }
  function fillsTable(fills, ref) {
    return h("div", { class: "scroll" }, h("table", null,
      h("thead", null, h("tr", null, ["时间", "方向", "价格", "数量", "level", "scope"].concat(HORIZONS.map(x => x + "s")).map((t, i) => h("th", { class: i >= 2 ? "r" : null }, t)))),
      h("tbody", null, fills.map(f => h("tr", null,
        h("td", null, fmtTime(f.ts_ns)), h("td", { class: f.side === "buy" ? "up" : "down" }, f.side === "buy" ? "买" : "卖"),
        h("td", { class: "r" }, f.price), h("td", { class: "r" }, f.qty), h("td", { class: "r" }, f.level === null ? "—" : f.level),
        h("td", { class: "r" }, f.scope || "—"),
        HORIZONS.map(hz => h("td", { class: "r" }, signed(f.markout_bps[hz + ":" + ref], 2))))))));
  }

  function drawCurve(el, curve) {
    if (!curve.length) { placeholder(el, "没有曲线（缺样本）。"); return; }
    const t = p => p.ts_ns / 1e6;
    const c = chartIn(el);
    c.setOption(baseOption({
      xAxis: timeAxis(),
      yAxis: [valueAxis("计价币"), valueAxis("持仓", { splitLine: { show: false } })],
      series: [
        line("合计", curve.map(p => [t(p), p.total]), { lineStyle: { width: 2 } }),
        line("已实现", curve.map(p => [t(p), p.realised])),
        line("未实现", curve.map(p => [t(p), p.unrealised])),
        line("手续费（负）", curve.map(p => [t(p), -p.fees])),
        line("持仓", curve.map(p => [t(p), p.position]), { yAxisIndex: 1, step: "end", lineStyle: { width: 1, type: "dashed" } }),
      ],
    }));
  }
  function drawPrice(el, curve, fills) {
    if (!curve.length) { fill(el); return; }
    const c = chartIn(el);
    const pts = side => fills.filter(f => f.side === side).map(f => [f.ts_ns / 1e6, Number(f.price)]);
    c.setOption(baseOption({
      grid: { left: 64, right: 56, top: 40, bottom: 30 },
      xAxis: timeAxis(), yAxis: valueAxis("价格"),
      series: [
        line("Bitget 中间价", curve.map(p => [p.ts_ns / 1e6, p.mid]), { color: css("--slate") }),
        { name: "买", type: "scatter", data: pts("buy"), symbolSize: 7, itemStyle: { color: css("--green") } },
        { name: "卖", type: "scatter", data: pts("sell"), symbolSize: 7, itemStyle: { color: css("--red") } },
      ],
    }));
  }
  function drawMarkout(el, runs, ref) {
    const c = chartIn(el);
    const series = [];
    runs.forEach(r => {
      const agg = r.m.aggregate || {};
      const mean = HORIZONS.map(hz => { const x = ((agg[String(hz)] || {})[ref] || {}).mean_bps; return isNum(x) ? Number(x) : null; });
      const med = HORIZONS.map(hz => { const x = ((agg[String(hz)] || {})[ref] || {}).median_bps; return isNum(x) ? Number(x) : null; });
      series.push(line((r.name ? r.name + " " : "") + "均值", mean, { showSymbol: true, lineStyle: { width: 2 } }));
      if (runs.length === 1) series.push(line("中位数", med, { showSymbol: true, lineStyle: { width: 1, type: "dashed" } }));
    });
    c.setOption(baseOption({
      xAxis: Object.assign({ type: "category", data: HORIZONS.map(x => x + "s") }, axisStyle()),
      yAxis: valueAxis("", { scale: false }),
      series: series.map((s, i) => i === 0 ? Object.assign(s, { markLine: { silent: true, symbol: "none", label: { show: false }, data: [{ yAxis: 0 }], lineStyle: { color: css("--slate"), type: "solid", width: 1 } } }) : s),
    }));
  }
  function drawScatter(el, fills, hz, ref) {
    const c = chartIn(el);
    const pts = side => fills.filter(f => f.side === side && isNum(f.markout_bps[hz + ":" + ref])).map(f => [f.ts_ns / 1e6, f.markout_bps[hz + ":" + ref]]);
    c.setOption(baseOption({
      tooltip: { trigger: "item", backgroundColor: css("--sheet"), borderColor: css("--rule"), textStyle: { color: css("--ink") } },
      xAxis: timeAxis(), yAxis: valueAxis("bps", { scale: false }),
      series: [
        { name: "买", type: "scatter", data: pts("buy"), symbolSize: 6, itemStyle: { color: css("--green") } },
        { name: "卖", type: "scatter", data: pts("sell"), symbolSize: 6, itemStyle: { color: css("--red") } },
      ],
    }));
  }

  /* ================= 对比 ================= */
  async function pageCompare(main, r) {
    const runs = await Promise.all(r.ids.map(id => api("/api/runs/" + encodeURIComponent(id))));
    const ref = state.prefs.ref || "bitget";
    const s = sheet("对比 " + runs.length + " 次运行", "markout 取各运行第一个合约；累计盈亏按运行开始后经过的时间对齐");
    add(s.head, h("span", { class: "spacer" }), seg([["bitget", "Bitget 中间价"], ["binance", "Binance 中间价"]], ref, k => { state.prefs.ref = k; savePrefs(); route(); }));
    add(s.body, runsTable(runs, null));
    const moBox = h("div", { class: "chart" });
    const curveBox = h("div", { class: "chart" });
    add(s.body, h("div", { class: "grid2" }, h("div", null, h("div", { class: "sec-head" }, h("h2", null, "markout 均值（bps）")), moBox),
      h("div", null, h("div", { class: "sec-head" }, h("h2", null, "累计盈亏（计价币）")), curveBox)));
    add(main, s);
    drawMarkout(moBox, runs.filter(x => firstSymbol(x) && x.markout[firstSymbol(x)]).map(x => ({ name: runName(x), m: x.markout[firstSymbol(x)] })), ref);
    const curves = await Promise.all(runs.map(x => firstSymbol(x)
      ? api("/api/runs/" + encodeURIComponent(x.run_id) + "/curve?symbol=" + encodeURIComponent(firstSymbol(x))) : Promise.resolve([])));
    const c = chartIn(curveBox);
    c.setOption(baseOption({
      xAxis: Object.assign({ type: "value", name: "小时", nameTextStyle: { color: css("--muted") } }, axisStyle()),
      yAxis: valueAxis(),
      series: curves.map((cv, i) => line(runName(runs[i]), cv.map(p => [(p.ts_ns - runs[i].started_ns) / 3.6e12, p.total]))),
    }));
  }

  route();
})();
