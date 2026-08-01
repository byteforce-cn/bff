// ============================================================
// 首页：BFF 健康检查 + 路由概览
// ============================================================

import { escapeHtml } from "../lib/utils";

// 根据 routes.yaml (routes_v2) 的静态定义
interface RouteSummary {
  path: string;
  methods: string[];
  auth: boolean;
  type: string;
  note: string;
}

const ROUTE_SUMMARY: RouteSummary[] = [
  { path: "/api/health", methods: ["GET"], auth: false, type: "static", note: "健康检查，返回版本号" },
  { path: "/api/users", methods: ["GET", "POST", "PUT", "DELETE"], auth: true, type: "proxy", note: "用户 CRUD → fakesvc:9091" },
  { path: "/api/orders", methods: ["GET", "POST", "DELETE"], auth: true, type: "proxy", note: "订单查询 → fakesvc:9091" },
  { path: "/api/echo", methods: ["GET"], auth: false, type: "pipeline", note: "快速验证 pipeline 引擎" },
  { path: "/api/dashboard", methods: ["GET"], auth: true, type: "pipeline", note: "聚合用户+订单数据" },
  { path: "/sse", methods: ["GET"], auth: false, type: "proxy (SSE)", note: "流式透传 → fakesvc:9091" },
  { path: "/ws", methods: ["GET"], auth: false, type: "proxy (WS)", note: "WebSocket 隧道 → fakesvc:9091" },
];

export function renderHome(el: HTMLElement) {
  el.innerHTML = `
    <h1 style="margin-bottom:24px">🧪 BFF 测试 SPA</h1>

    <div class="card">
      <h2>🔗 BFF 健康检查</h2>
      <div id="health-status"><span class="status loading">检测中…</span></div>
    </div>

    <div class="card">
      <h2>📡 路由快速探测</h2>
      <p style="color:var(--muted);font-size:13px;margin-bottom:12px">
        对已注册路由发起快速 HEAD/GET，检查各端点可达性。
      </p>
      <div id="route-probe"></div>
    </div>

    <div class="card">
      <h2>📖 路由一览 (routes.yaml)</h2>
      <table>
        <thead><tr>
          <th>路径</th><th>Methods</th><th>认证</th><th>类型</th><th>说明</th>
        </tr></thead>
        <tbody>${ROUTE_SUMMARY.map((r) => `
          <tr>
            <td><code style="font-size:12px">${escapeHtml(r.path)}</code></td>
            <td>${r.methods.map((m) => `<span class="method-badge">${m}</span>`).join(" ")}</td>
            <td>${r.auth ? '<span style="color:var(--success)">✓</span>' : '<span style="color:var(--muted)">—</span>'}</td>
            <td>${escapeHtml(r.type)}</td>
            <td style="font-size:12px;color:var(--muted)">${escapeHtml(r.note)}</td>
          </tr>`).join("")}
        </tbody>
      </table>
    </div>

    <div class="card">
      <h2>🧭 测试页面导航</h2>
      <table>
        <tr><th>页面</th><th>测试内容</th></tr>
        <tr><td><a href="#/login">🔐 登录</a></td><td>OIDC 登录流程、Session 管理</td></tr>
        <tr><td><a href="#/dashboard">📊 仪表盘</a></td><td>Pipeline 编排 (/api/echo, /api/dashboard)</td></tr>
        <tr><td><a href="#/proxy">🔀 代理</a></td><td>HTTP/SSE/WebSocket 全功能代理测试</td></tr>
      </table>
    </div>
  `;

  checkHealth();
  probeRoutes();
}

async function checkHealth() {
  const el = document.getElementById("health-status")!;
  try {
    const resp = await fetch("/api/health");
    const json = await resp.json();
    el.innerHTML = `
      <span class="status ok">BFF 运行正常</span>
      <pre style="margin-top:8px;font-size:12px">${escapeHtml(JSON.stringify(json, null, 2))}</pre>`;
  } catch {
    el.innerHTML = `<span class="status err">无法连接 BFF（请确认 BFF 已启动在 8080 端口）</span>`;
  }
}

async function probeRoutes() {
  const el = document.getElementById("route-probe")!;
  el.innerHTML = ROUTE_SUMMARY.map((r) =>
    `<div style="display:flex;align-items:center;gap:12px;padding:4px 0;border-bottom:1px solid var(--border)">
      <code style="font-size:12px;min-width:140px">${escapeHtml(r.path)}</code>
      <span id="probe-${CSS.escape(r.path)}"><span class="status loading">探测中…</span></span>
    </div>`
  ).join("");

  // 并行探测所有路由
  for (const r of ROUTE_SUMMARY) {
    const probeEl = document.getElementById(`probe-${CSS.escape(r.path)}`);
    if (!probeEl) continue;
    try {
      const method = r.methods.includes("GET") ? "GET" : "HEAD";
      const resp = await fetch(r.path, { method });
      probeEl.innerHTML = `<span class="status ok">${resp.status} OK</span>`;
    } catch {
      probeEl.innerHTML = `<span class="status err">不可达</span>`;
    }
  }
}
