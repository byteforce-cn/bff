// ============================================================
// 代理测试页 — 全功能代理联调（HTTP / SSE / WebSocket）
//
// TDD 重构 (2026-07-30):
// - 路由数据从 GET /admin/api/routes/v2 获取（非硬编码）
// - Tab 切换布局（HTTP / SSE / WS）
// - HTTP: 请求体 + 自定义 Headers + 响应元信息 + 历史
// - SSE: 暂停/清空/过滤/自动滚动开关
// - WS: 动态端点 + Enter 发送 + 收发视觉区分
// ============================================================

import {
  escapeHtml,
  copyToClipboard,
  formatBytes,
  formatDuration,
  formatElapsed,
  formatError,
  latencyColor,
} from "../lib/utils";
import { fetchRoutesV2, fetchHealth, type RouteDef } from "../lib/api";

// ---- 全局状态 ----

let wsConnection: WebSocket | null = null;
let eventSource: EventSource | null = null;
let ssePaused = false;
let sseAutoScroll = true;
let sseFilterType = "";
let wsConnectedAt = 0;
let wsSentCount = 0;
let wsRecvCount = 0;
let sseEventCount = 0;
let requestHistory: {
  id: number;
  timestamp: number;
  path: string;
  method: string;
  status?: number;
  durationMs?: number;
  sizeBytes?: number;
}[] = [];
let historyIdSeq = 0;

let cachedRoutes: RouteDef[] = [];

// ============================================================
// 主渲染
// ============================================================

export function renderProxy(el: HTMLElement) {
  el.innerHTML = `
    <h1 style="margin-bottom:16px">🔀 全功能代理测试</h1>

    <!-- 路由状态卡片（常驻顶部） -->
    <div class="card" id="route-card">
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:12px">
        <h2 style="margin:0">📋 路由状态</h2>
        <div style="display:flex;gap:8px;align-items:center">
          <span id="bff-status"><span class="status loading">检测中…</span></span>
          <button class="btn" id="route-refresh-btn">🔄 刷新</button>
        </div>
      </div>
      <div id="route-filter" style="margin-bottom:12px;display:flex;gap:6px;flex-wrap:wrap">
        <button class="chip active" data-filter="all">全部</button>
        <button class="chip" data-filter="proxy">🔗 代理</button>
        <button class="chip" data-filter="pipeline">⚙ 编排</button>
        <button class="chip" data-filter="script">📜 脚本</button>
        <button class="chip" data-filter="static">📄 静态</button>
      </div>
      <div id="route-list"><span class="status loading">加载路由…</span></div>
    </div>

    <!-- Tab 切换 -->
    <div class="card" style="padding:12px 24px">
      <div style="display:flex;gap:4px" id="tab-bar">
        <button class="tab active" data-tab="http">🔀 HTTP REST</button>
        <button class="tab" data-tab="sse">📡 SSE 流</button>
        <button class="tab" data-tab="ws">🔌 WebSocket</button>
      </div>
    </div>

    <!-- Tab 内容区 -->
    <div id="tab-content" style="min-height:400px"></div>
  `;

  // 初始化
  loadRoutes();
  const hashTab = (window.location.hash.slice(1).split("/")[2]) || "http";
  const validTabs = ["http", "sse", "ws"];
  switchTab(validTabs.includes(hashTab) ? hashTab : "http");

  // 事件绑定
  document.getElementById("route-refresh-btn")!.addEventListener("click", loadRoutes);
  document.querySelectorAll("#route-filter .chip").forEach((chip) => {
    chip.addEventListener("click", (e) => {
      const filter = (e.target as HTMLElement).dataset.filter || "all";
      document.querySelectorAll("#route-filter .chip").forEach((c) => c.classList.remove("active"));
      (e.target as HTMLElement).classList.add("active");
      renderRouteTable(filter);
    });
  });
  document.querySelectorAll("#tab-bar .tab").forEach((tab) => {
    tab.addEventListener("click", (e) => {
      const name = (e.target as HTMLElement).dataset.tab || "http";
      switchTab(name);
    });
  });
}

// ============================================================
// Tab 切换
// ============================================================

function switchTab(name: string) {
  document.querySelectorAll("#tab-bar .tab").forEach((t) => t.classList.remove("active"));
  const target = document.querySelector(`#tab-bar .tab[data-tab="${name}"]`);
  if (target) target.classList.add("active");

  const newHash = `#/proxy/${name}`;
  if (window.location.hash !== newHash) {
    window.location.hash = newHash;
  }

  const content = document.getElementById("tab-content")!;
  switch (name) {
    case "http": renderHttpTab(content); break;
    case "sse": renderSseTab(content); break;
    case "ws": renderWsTab(content); break;
  }
}

// ============================================================
// 路由状态
// ============================================================

async function loadRoutes() {
  const statusEl = document.getElementById("bff-status")!;
  const listEl = document.getElementById("route-list")!;

  try {
    const [healthy, routes] = await Promise.all([
      fetchHealth(),
      fetchRoutesV2().catch(() => [] as RouteDef[]),
    ]);

    cachedRoutes = routes;

    statusEl.innerHTML = healthy
      ? '<span class="status ok">BFF 在线 ✅</span>'
      : '<span class="status err">BFF 离线 ❌</span>';

    renderRouteTable("all");
  } catch {
    statusEl.innerHTML = '<span class="status err">无法连接 BFF ❌</span>';
    listEl.innerHTML =
      '<p style="color:var(--muted);font-size:13px">无法获取路由列表，请检查 BFF 是否已启动</p>';
  }
}

function renderRouteTable(filter: string) {
  const listEl = document.getElementById("route-list")!;

  let routes = cachedRoutes;
  if (filter !== "all") {
    routes = routes.filter((r) => r.type === filter);
  }

  if (routes.length === 0) {
    listEl.innerHTML = '<p style="color:var(--muted);font-size:13px">暂无匹配路由</p>';
    return;
  }

  const typeLabel = (t: string): string => {
    switch (t) {
      case "proxy": return "🔗 代理";
      case "pipeline": return "⚙ 编排";
      case "script": return "📜 脚本";
      case "static": return "📄 静态";
      default: return t;
    }
  };

  const proxyModeBadge = (mode?: string): string => {
    if (!mode || mode === "http") return '<span class="badge badge-http">HTTP</span>';
    if (mode === "sse") return '<span class="badge badge-sse">SSE ⚡</span>';
    if (mode === "websocket") return '<span class="badge badge-ws">WS 🔌</span>';
    if (mode === "auto") return '<span class="badge badge-auto">AUTO</span>';
    return `<span class="badge">${escapeHtml(mode)}</span>`;
  };

  listEl.innerHTML = `
    <table>
      <thead><tr>
        <th>路径</th><th>Methods</th><th>类型</th>
        <th>代理模式</th><th>上游地址</th><th>认证</th>
      </tr></thead>
      <tbody>${routes.map((r) => `
        <tr class="route-row clickable" data-path="${escapeHtml(r.path)}" data-type="${r.type}" data-proxy-mode="${r.config.proxy_mode || "http"}">
          <td><code style="font-size:12px">${escapeHtml(r.path)}</code></td>
          <td>${(r.methods.length > 0 ? r.methods : ["ALL"]).map((m) => `<span class="method-badge">${m}</span>`).join(" ")}</td>
          <td>${typeLabel(r.type)}</td>
          <td>${proxyModeBadge(r.config.proxy_mode)}</td>
          <td style="color:var(--muted);font-size:12px;max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(r.config.upstream || "—")}</td>
          <td>${r.auth_required ? '<span style="color:var(--success)">✓</span>' : '<span style="color:var(--muted)">—</span>'}</td>
        </tr>`).join("")}
      </tbody>
    </table>
  `;

  // 点击行 → 切换 Tab + 填充路径
  listEl.querySelectorAll(".route-row").forEach((row) => {
    row.addEventListener("click", () => {
      const path = row.getAttribute("data-path") || "";
      const proxyMode = row.getAttribute("data-proxy-mode") || "http";
      if (proxyMode === "sse") {
        switchTab("sse");
        setTimeout(() => { const inp = document.getElementById("sse-path") as HTMLInputElement; if (inp) inp.value = path; }, 50);
      } else if (proxyMode === "websocket") {
        switchTab("ws");
        setTimeout(() => {
          const sel = document.getElementById("ws-endpoint") as HTMLSelectElement;
          if (sel) {
            const opt = document.createElement("option");
            opt.value = path;
            opt.textContent = path;
            sel.appendChild(opt);
            sel.value = path;
          }
        }, 50);
      } else {
        switchTab("http");
        setTimeout(() => { const inp = document.getElementById("proxy-path") as HTMLInputElement; if (inp) inp.value = path; }, 50);
      }
    });
  });
}

// ============================================================
// HTTP REST Tab
// ============================================================

function renderHttpTab(container: HTMLElement) {
  container.innerHTML = `
    <div class="card">
      <h2>🔀 HTTP REST 代理</h2>
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:12px">
        <input id="proxy-path" type="text" value="/api/users"
          style="flex:1;min-width:200px;padding:8px 12px;border:1px solid var(--border);border-radius:6px;font-size:14px" placeholder="请求路径" />
        <select id="proxy-method" style="padding:6px 12px;border-radius:6px;border:1px solid var(--border);font-size:14px">
          <option>GET</option><option>POST</option><option>PUT</option><option>PATCH</option><option>DELETE</option>
        </select>
        <button class="btn btn-primary" id="proxy-send-btn">▶ 发送</button>
      </div>
      <div id="quick-buttons" style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:12px"></div>
      <details style="margin-bottom:12px">
        <summary style="cursor:pointer;font-size:13px;color:var(--muted)">📋 请求头 (可选)</summary>
        <div id="headers-editor" style="margin-top:8px">
          <div class="header-row" style="display:flex;gap:8px;margin-bottom:4px">
            <input type="text" placeholder="Header Name" value="Content-Type" style="flex:1;padding:6px 8px;border:1px solid var(--border);border-radius:4px;font-size:12px" />
            <input type="text" placeholder="Header Value" value="application/json" style="flex:1;padding:6px 8px;border:1px solid var(--border);border-radius:4px;font-size:12px" />
            <button class="btn btn-sm btn-del-header" style="padding:4px 8px;font-size:12px">🗑️</button>
          </div>
        </div>
        <button class="btn btn-sm" id="add-header-btn" style="margin-top:4px;font-size:12px">+ 添加</button>
      </details>
      <div id="body-section" style="display:none;margin-bottom:12px">
        <label style="font-size:13px;color:var(--muted)">📝 请求体 (JSON)</label>
        <textarea id="proxy-body" rows="6" placeholder='{"key": "value"}'
          style="width:100%;padding:8px 12px;border:1px solid var(--border);border-radius:6px;font-size:13px;font-family:monospace;resize:vertical;margin-top:4px"></textarea>
      </div>
      <div id="proxy-result-section">
        <div id="proxy-result" style="color:var(--muted);font-size:13px">点击"发送"发起请求…</div>
      </div>
    </div>
  `;

  document.getElementById("proxy-send-btn")!.addEventListener("click", sendProxy);
  document.getElementById("proxy-path")!.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).ctrlKey && (e as KeyboardEvent).key === "Enter") sendProxy();
  });
  document.getElementById("proxy-method")!.addEventListener("change", (e) => {
    const m = (e.target as HTMLSelectElement).value;
    document.getElementById("body-section")!.style.display = ["POST", "PUT", "PATCH"].includes(m) ? "block" : "none";
  });

  renderQuickButtons();
  bindHeaderEditor();
}

function renderQuickButtons() {
  const ctr = document.getElementById("quick-buttons")!;
  const proxyRoutes = cachedRoutes.filter((r) => r.type === "proxy" && r.config.proxy_mode !== "websocket");
  if (proxyRoutes.length === 0) {
    ctr.innerHTML = `
      <button class="btn btn-sm quick-btn" data-path="/api/health" data-method="GET">GET /api/health</button>
      <button class="btn btn-sm quick-btn" data-path="/api/users" data-method="GET">GET /api/users</button>
      <button class="btn btn-sm quick-btn" data-path="/api/orders" data-method="GET">GET /api/orders</button>
      <button class="btn btn-sm quick-btn" data-path="/api/echo" data-method="GET">GET /api/echo</button>`;
  } else {
    ctr.innerHTML = proxyRoutes.slice(0, 6).map((r) => {
      const m = r.methods.length > 0 ? r.methods[0] : "GET";
      return `<button class="btn btn-sm quick-btn" data-path="${escapeHtml(r.path)}" data-method="${m}">${m} ${escapeHtml(r.path)}</button>`;
    }).join("");
  }
  ctr.querySelectorAll(".quick-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const p = btn.getAttribute("data-path") || "/";
      const m = btn.getAttribute("data-method") || "GET";
      (document.getElementById("proxy-path") as HTMLInputElement).value = p;
      (document.getElementById("proxy-method") as HTMLSelectElement).value = m;
      document.getElementById("body-section")!.style.display = ["POST", "PUT", "PATCH"].includes(m) ? "block" : "none";
      sendProxy();
    });
  });
}

function bindHeaderEditor() {
  document.getElementById("add-header-btn")!.addEventListener("click", () => {
    const editor = document.getElementById("headers-editor")!;
    const row = document.createElement("div");
    row.className = "header-row";
    row.style.cssText = "display:flex;gap:8px;margin-bottom:4px";
    row.innerHTML = `
      <input type="text" placeholder="Header Name" style="flex:1;padding:6px 8px;border:1px solid var(--border);border-radius:4px;font-size:12px" />
      <input type="text" placeholder="Header Value" style="flex:1;padding:6px 8px;border:1px solid var(--border);border-radius:4px;font-size:12px" />
      <button class="btn btn-sm btn-del-header" style="padding:4px 8px;font-size:12px">🗑️</button>`;
    row.querySelector(".btn-del-header")!.addEventListener("click", () => row.remove());
    editor.appendChild(row);
  });
  document.querySelectorAll(".btn-del-header").forEach((b) => {
    b.addEventListener("click", () => (b.closest(".header-row") as HTMLElement)?.remove());
  });
}

async function sendProxy() {
  const resultEl = document.getElementById("proxy-result")!;
  const pathInput = document.getElementById("proxy-path") as HTMLInputElement;
  const methodSelect = document.getElementById("proxy-method") as HTMLSelectElement;
  const path = pathInput.value;
  const method = methodSelect.value;

  resultEl.innerHTML = `<span class="status loading">发送 ${method} ${escapeHtml(path)}…</span>`;

  const headers: Record<string, string> = {};
  document.querySelectorAll("#headers-editor .header-row").forEach((row) => {
    const inputs = row.querySelectorAll("input");
    const name = (inputs[0] as HTMLInputElement).value.trim();
    const value = (inputs[1] as HTMLInputElement).value.trim();
    if (name && value) headers[name] = value;
  });

  const opts: RequestInit = { method, headers: new Headers(headers) };
  if (["POST", "PUT", "PATCH"].includes(method)) {
    const bodyEl = document.getElementById("proxy-body") as HTMLTextAreaElement;
    if (bodyEl && bodyEl.value.trim()) {
      opts.body = bodyEl.value;
      if (!headers["content-type"] && !headers["Content-Type"]) {
        (opts.headers as Headers).set("Content-Type", "application/json");
      }
    }
  }

  const start = performance.now();
  try {
    const resp = await fetch(path, opts);
    const elapsed = performance.now() - start;
    const body = await resp.text();
    const sizeBytes = new Blob([body]).size;

    let formatted: string;
    try { formatted = JSON.stringify(JSON.parse(body), null, 2); } catch { formatted = body; }

    const statusClass = resp.ok ? "ok" : "err";
    const colorClass = latencyColor(elapsed);

    const respHeaders: Record<string, string> = {};
    resp.headers.forEach((v, k) => { respHeaders[k] = v; });

    addHistory(path, method, resp.status, elapsed, sizeBytes);

    resultEl.innerHTML = `
      <div style="display:flex;align-items:center;gap:12px;margin-bottom:8px;flex-wrap:wrap">
        <span class="status ${statusClass}">HTTP ${resp.status}</span>
        <span class="latency ${colorClass}">⏱ ${formatDuration(elapsed)}</span>
        <span style="font-size:12px;color:var(--muted)">📦 ${formatBytes(sizeBytes)}</span>
        <button class="btn btn-sm copy-btn" data-text="${escapeAttr(formatted)}" style="margin-left:auto;font-size:12px">📋 复制</button>
      </div>
      <details style="margin-bottom:8px">
        <summary style="cursor:pointer;font-size:12px;color:var(--muted)">响应头</summary>
        <pre style="margin-top:4px;max-height:120px;font-size:12px">${escapeHtml(JSON.stringify(respHeaders, null, 2))}</pre>
      </details>
      <details open>
        <summary style="cursor:pointer;font-size:12px;color:var(--muted)">响应体</summary>
        <pre style="margin-top:4px;max-height:350px;font-size:12px">${escapeHtml(formatted)}</pre>
      </details>`;

    resultEl.querySelectorAll(".copy-btn").forEach((btn) => {
      btn.addEventListener("click", async () => {
        const text = btn.getAttribute("data-text") || "";
        const ok = await copyToClipboard(text);
        btn.textContent = ok ? "✅ 已复制" : "❌ 失败";
        setTimeout(() => { btn.textContent = "📋 复制"; }, 2000);
      });
    });
  } catch (e) {
    const elapsed = performance.now() - start;
    addHistory(path, method, undefined, elapsed, 0);
    resultEl.innerHTML = `
      <span class="status err">请求失败</span>
      <span class="latency latency-err">⏱ ${formatDuration(elapsed)}</span>
      <pre style="margin-top:8px;font-size:12px">${escapeHtml(formatError(e))}</pre>`;
  }

  renderHistory();
}

// ============================================================
// SSE Tab
// ============================================================

function renderSseTab(container: HTMLElement) {
  container.innerHTML = `
    <div class="card">
      <h2>📡 SSE 流式透传</h2>
      <p style="color:var(--muted);margin-bottom:12px;font-size:13px">BFF 逐 chunk 透传上游 SSE 事件流。</p>
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:12px">
        <input id="sse-path" type="text" value="/sse"
          style="flex:1;min-width:200px;padding:8px 12px;border:1px solid var(--border);border-radius:6px;font-size:14px" />
        <button class="btn btn-primary" id="sse-connect-btn">🔗 连接</button>
        <button class="btn" id="sse-pause-btn" disabled>⏸ 暂停</button>
        <button class="btn" id="sse-clear-btn">🗑 清空</button>
        <button class="btn btn-danger" id="sse-close-btn" disabled>断开</button>
      </div>
      <div id="sse-status-bar" style="margin-bottom:8px;display:flex;gap:16px;align-items:center;flex-wrap:wrap;font-size:12px;color:var(--muted)">
        <span id="sse-conn-status">● 未连接</span>
        <span id="sse-elapsed">运行时间 00:00:00</span>
        <span id="sse-event-count">收到 0 条事件</span>
      </div>
      <div style="display:flex;gap:16px;align-items:center;margin-bottom:8px">
        <label style="font-size:12px;display:flex;align-items:center;gap:4px;cursor:pointer">
          <input type="checkbox" id="sse-autoscroll" checked /> 自动滚动
        </label>
      </div>
      <div id="sse-output" style="background:#1e293b;border-radius:6px;padding:12px;min-height:350px;max-height:500px;overflow-y:auto;font-family:monospace;font-size:13px;color:#e2e8f0">
        <span style="color:var(--muted)">点击"连接"开始接收 SSE 事件…</span>
      </div>
    </div>`;

  document.getElementById("sse-connect-btn")!.addEventListener("click", connectSSE);
  document.getElementById("sse-close-btn")!.addEventListener("click", closeSSE);
  document.getElementById("sse-pause-btn")!.addEventListener("click", toggleSsePause);
  document.getElementById("sse-clear-btn")!.addEventListener("click", clearSseOutput);
  document.getElementById("sse-autoscroll")!.addEventListener("change", (e) => {
    sseAutoScroll = (e.target as HTMLInputElement).checked;
  });
  document.getElementById("sse-path")!.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") connectSSE();
  });

  startSseTimer();
}

let sseTimerId: ReturnType<typeof setInterval> | null = null;
let sseStartTime = 0;

function startSseTimer() {
  if (sseTimerId) clearInterval(sseTimerId);
  sseTimerId = setInterval(() => {
    const el = document.getElementById("sse-elapsed");
    if (el && sseStartTime > 0) {
      el.textContent = `运行时间 ${formatElapsed(Math.floor((Date.now() - sseStartTime) / 1000))}`;
    }
  }, 1000);
}

function connectSSE() {
  const pathInput = document.getElementById("sse-path") as HTMLInputElement;
  const path = pathInput.value;
  const outputEl = document.getElementById("sse-output")!;
  const connectBtn = document.getElementById("sse-connect-btn") as HTMLButtonElement;
  const pauseBtn = document.getElementById("sse-pause-btn") as HTMLButtonElement;
  const closeBtn = document.getElementById("sse-close-btn") as HTMLButtonElement;

  closeSSE();

  outputEl.innerHTML = "";
  ssePaused = false;
  sseFilterType = "";
  sseEventCount = 0;
  sseStartTime = Date.now();
  appendSSE("system", `🔗 连接 ${path} …`);

  eventSource = new EventSource(path);

  eventSource.onopen = () => {
    appendSSE("system", "✅ SSE 连接已建立");
    connectBtn.disabled = true;
    pauseBtn.disabled = false;
    closeBtn.disabled = false;
    pauseBtn.textContent = "⏸ 暂停";
    updateSseStatus("● 已连接", "var(--success)");
  };

  eventSource.onmessage = (event) => {
    if (ssePaused) return;
    sseEventCount++;
    updateSseEventCount();
    appendSSE("message", event.data);
  };

  eventSource.addEventListener("clock", (event: Event) => {
    if (ssePaused || (sseFilterType && sseFilterType !== "clock")) return;
    sseEventCount++;
    updateSseEventCount();
    appendSSE("clock", (event as MessageEvent).data);
  });

  eventSource.onerror = () => {
    appendSSE("error", "❌ SSE 连接错误/关闭");
    connectBtn.disabled = false;
    pauseBtn.disabled = true;
    closeBtn.disabled = true;
    eventSource = null;
    sseStartTime = 0;
    updateSseStatus("● 已断开", "var(--danger)");
  };
}

function closeSSE() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
    sseStartTime = 0;
    appendSSE("system", "🔌 已手动断开 SSE");
  }
  const connectBtn = document.getElementById("sse-connect-btn") as HTMLButtonElement;
  const pauseBtn = document.getElementById("sse-pause-btn") as HTMLButtonElement;
  const closeBtn = document.getElementById("sse-close-btn") as HTMLButtonElement;
  if (connectBtn) connectBtn.disabled = false;
  if (pauseBtn) { pauseBtn.disabled = true; pauseBtn.textContent = "⏸ 暂停"; }
  if (closeBtn) closeBtn.disabled = true;
  ssePaused = false;
  updateSseStatus("● 未连接", "var(--muted)");
}

function toggleSsePause() {
  ssePaused = !ssePaused;
  const btn = document.getElementById("sse-pause-btn") as HTMLButtonElement;
  if (btn) btn.textContent = ssePaused ? "▶ 恢复" : "⏸ 暂停";
  appendSSE("system", ssePaused ? "⏸ 已暂停渲染（连接未断开）" : "▶ 已恢复接收");
}

function clearSseOutput() {
  const el = document.getElementById("sse-output");
  if (el) el.innerHTML = "";
  sseEventCount = 0;
  updateSseEventCount();
}

function updateSseStatus(text: string, color: string) {
  const el = document.getElementById("sse-conn-status");
  if (el) { el.textContent = text; el.style.color = color; }
}

function updateSseEventCount() {
  const el = document.getElementById("sse-event-count");
  if (el) el.textContent = `收到 ${sseEventCount} 条事件`;
}

function appendSSE(type: string, data: string) {
  const outputEl = document.getElementById("sse-output");
  if (!outputEl) return;

  const colors: Record<string, string> = {
    system: "#94a3b8", message: "#60a5fa", clock: "#34d399", error: "#f87171",
  };

  let formatted = data;
  try { formatted = JSON.stringify(JSON.parse(data), null, 2); } catch { /* raw */ }

  const line = document.createElement("div");
  line.style.padding = "2px 0";
  line.innerHTML = `<span style="color:#64748b">[${new Date().toLocaleTimeString()}]</span> <span style="color:${colors[type] || colors.message}">📩 ${escapeHtml(formatted)}</span>`;
  outputEl.appendChild(line);

  if (sseAutoScroll) outputEl.scrollTop = outputEl.scrollHeight;
}

// ============================================================
// WebSocket Tab
// ============================================================

function renderWsTab(container: HTMLElement) {
  const wsRoutes = cachedRoutes.filter((r) => r.config.proxy_mode === "websocket").map((r) => r.path);
  const endpoints = wsRoutes.length > 0
    ? wsRoutes.map((p) => `<option value="${escapeHtml(p)}">${escapeHtml(p)}</option>`).join("")
    : `<option value="/ws">/ws (隧道)</option>`;

  container.innerHTML = `
    <div class="card">
      <h2>🔌 WebSocket 双向隧道</h2>
      <p style="color:var(--muted);margin-bottom:12px;font-size:13px">BFF 建立客户端↔上游的双向 WS 隧道。</p>
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:12px">
        <select id="ws-endpoint" style="flex:1;min-width:200px;padding:8px 12px;border:1px solid var(--border);border-radius:6px;font-size:14px">${endpoints}</select>
        <button class="btn btn-primary" id="ws-connect-btn">🔗 连接</button>
        <button class="btn btn-danger" id="ws-close-btn" disabled>🔌 断开</button>
        <button class="btn" id="ws-clear-btn">🗑 清空</button>
      </div>
      <div id="ws-status-bar" style="margin-bottom:8px;display:flex;gap:16px;align-items:center;flex-wrap:wrap;font-size:12px;color:var(--muted)">
        <span id="ws-conn-status">● 未连接</span>
        <span id="ws-elapsed">运行时间 00:00:00</span>
        <span id="ws-count">收发 0/0 条</span>
      </div>
      <div style="display:flex;gap:8px;margin-bottom:8px">
        <input id="ws-message" type="text" value='{"type":"ping"}'
          style="flex:1;padding:8px 12px;border:1px solid var(--border);border-radius:6px;font-size:14px;font-family:monospace"
          placeholder="输入消息, Enter 发送 / Shift+Enter 换行" />
        <button class="btn btn-primary" id="ws-send-btn" disabled>📤 发送</button>
      </div>
      <div style="margin-bottom:8px;display:flex;gap:16px;align-items:center;font-size:12px;color:var(--muted)">
        <label style="display:flex;align-items:center;gap:4px;cursor:pointer">
          <input type="checkbox" id="ws-enter-send" checked /> Enter 发送
        </label>
        <label style="display:flex;align-items:center;gap:4px;cursor:pointer">
          <input type="checkbox" id="ws-json-format" checked /> JSON 格式化
        </label>
      </div>
      <div id="ws-output" style="background:#1e293b;border-radius:6px;padding:12px;min-height:350px;max-height:500px;overflow-y:auto;font-family:monospace;font-size:13px;color:#e2e8f0">
        <span style="color:var(--muted)">点击"连接"建立 WebSocket 隧道…</span>
      </div>
    </div>`;

  document.getElementById("ws-connect-btn")!.addEventListener("click", connectWS);
  document.getElementById("ws-close-btn")!.addEventListener("click", closeWS);
  document.getElementById("ws-clear-btn")!.addEventListener("click", () => {
    const el = document.getElementById("ws-output"); if (el) el.innerHTML = "";
    wsSentCount = 0; wsRecvCount = 0; updateWsCount();
  });
  document.getElementById("ws-send-btn")!.addEventListener("click", sendWS);

  const msgInput = document.getElementById("ws-message") as HTMLInputElement;
  msgInput.addEventListener("keydown", (e) => {
    const enterSend = (document.getElementById("ws-enter-send") as HTMLInputElement).checked;
    if ((e as KeyboardEvent).key === "Enter" && enterSend && !(e as KeyboardEvent).shiftKey) {
      e.preventDefault();
      sendWS();
    }
  });

  startWsTimer();
}

let wsTimerId: ReturnType<typeof setInterval> | null = null;

function startWsTimer() {
  if (wsTimerId) clearInterval(wsTimerId);
  wsTimerId = setInterval(() => {
    const el = document.getElementById("ws-elapsed");
    if (el && wsConnectedAt > 0) {
      el.textContent = `运行时间 ${formatElapsed(Math.floor((Date.now() - wsConnectedAt) / 1000))}`;
    }
  }, 1000);
}

function connectWS() {
  const endpointSelect = document.getElementById("ws-endpoint") as HTMLSelectElement;
  const endpoint = endpointSelect.value;
  const connectBtn = document.getElementById("ws-connect-btn") as HTMLButtonElement;
  const closeBtn = document.getElementById("ws-close-btn") as HTMLButtonElement;
  const sendBtn = document.getElementById("ws-send-btn") as HTMLButtonElement;
  const outputEl = document.getElementById("ws-output")!;

  closeWS();

  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  const url = `${protocol}//${location.host}${endpoint}`;

  outputEl.innerHTML = "";
  wsSentCount = 0;
  wsRecvCount = 0;
  appendWS("sys", `🔗 连接 ${url} …`, "");

  try {
    wsConnection = new WebSocket(url);
    wsConnectedAt = Date.now();

    wsConnection.onopen = () => {
      appendWS("sys", "✅ WebSocket 隧道已建立", "");
      connectBtn.disabled = true;
      closeBtn.disabled = false;
      sendBtn.disabled = false;
      updateWsStatus("● 已连接", "var(--success)");
    };

    wsConnection.onmessage = (event) => {
      wsRecvCount++;
      updateWsCount();
      let formatted = event.data;
      const jsonFormat = (document.getElementById("ws-json-format") as HTMLInputElement)?.checked ?? true;
      if (jsonFormat) {
        try { formatted = JSON.stringify(JSON.parse(event.data), null, 2); } catch { /* raw */ }
      }
      appendWS("recv", formatted, "←");
    };

    wsConnection.onerror = () => {
      appendWS("sys", "⚠️ WebSocket 错误", "");
    };

    wsConnection.onclose = (event) => {
      appendWS("sys", `🔌 隧道关闭 (code=${event.code}${event.reason ? `, reason=${event.reason}` : ""})`, "");
      connectBtn.disabled = false;
      closeBtn.disabled = true;
      sendBtn.disabled = true;
      wsConnection = null;
      wsConnectedAt = 0;
      updateWsStatus("● 已断开", "var(--danger)");
      updateWsCount();
    };
  } catch (e) {
    appendWS("sys", `❌ 连接失败: ${formatError(e)}`, "");
    connectBtn.disabled = false;
  }
}

function closeWS() {
  if (wsConnection) {
    wsConnection.close();
    wsConnection = null;
    wsConnectedAt = 0;
    appendWS("sys", "🔌 已手动断开 WebSocket", "");
  }
  const connectBtn = document.getElementById("ws-connect-btn") as HTMLButtonElement;
  const closeBtn = document.getElementById("ws-close-btn") as HTMLButtonElement;
  const sendBtn = document.getElementById("ws-send-btn") as HTMLButtonElement;
  if (connectBtn) connectBtn.disabled = false;
  if (closeBtn) closeBtn.disabled = true;
  if (sendBtn) sendBtn.disabled = true;
  updateWsStatus("● 未连接", "var(--muted)");
}

function sendWS() {
  if (!wsConnection || wsConnection.readyState !== WebSocket.OPEN) {
    appendWS("sys", "⚠️ 未连接", "");
    return;
  }
  const msgInput = document.getElementById("ws-message") as HTMLInputElement;
  const msg = msgInput.value;
  wsConnection.send(msg);
  wsSentCount++;
  updateWsCount();

  let display = msg;
  const jsonFormat = (document.getElementById("ws-json-format") as HTMLInputElement)?.checked ?? true;
  if (jsonFormat) {
    try { display = JSON.stringify(JSON.parse(msg), null, 2); } catch { /* raw */ }
  }
  appendWS("sent", display, "→");
}

function updateWsStatus(text: string, color: string) {
  const el = document.getElementById("ws-conn-status");
  if (el) { el.textContent = text; el.style.color = color; }
}

function updateWsCount() {
  const el = document.getElementById("ws-count");
  if (el) el.textContent = `收发 ${wsSentCount}/${wsRecvCount} 条`;
}

function appendWS(dir: "sent" | "recv" | "sys", msg: string, arrow: string) {
  const outputEl = document.getElementById("ws-output");
  if (!outputEl) return;

  const colors: Record<string, string> = {
    sent: "#34d399", recv: "#60a5fa", sys: "#94a3b8",
  };

  const line = document.createElement("div");
  line.style.padding = "2px 0";
  line.innerHTML = `<span style="color:#64748b">[${new Date().toLocaleTimeString()}]</span> <span style="color:${colors[dir]}">${arrow} ${escapeHtml(msg)}</span>`;
  outputEl.appendChild(line);
  outputEl.scrollTop = outputEl.scrollHeight;
}

// ============================================================
// 请求历史
// ============================================================

function addHistory(path: string, method: string, status?: number, durationMs?: number, sizeBytes?: number) {
  requestHistory.unshift({
    id: ++historyIdSeq,
    timestamp: Date.now(),
    path, method, status, durationMs, sizeBytes,
  });
  if (requestHistory.length > 10) requestHistory.pop();
}

function renderHistory() {
  const section = document.getElementById("proxy-result-section");
  if (!section || requestHistory.length === 0) return;

  const existing = document.getElementById("request-history");
  if (existing) existing.remove();

  const html = `
    <div id="request-history" style="margin-top:16px;border-top:1px solid var(--border);padding-top:12px">
      <details>
        <summary style="cursor:pointer;font-size:13px;color:var(--muted)">📜 历史 (最近 ${requestHistory.length} 条)</summary>
        <div style="margin-top:8px">
          ${requestHistory.map((h) => `
            <div class="history-row" style="display:flex;align-items:center;gap:12px;padding:4px 0;font-size:12px;border-bottom:1px solid var(--border);cursor:pointer"
                 data-path="${escapeHtml(h.path)}" data-method="${h.method}">
              <span style="color:var(--muted);min-width:36px">${h.method}</span>
              <code style="flex:1;font-size:11px">${escapeHtml(h.path)}</code>
              ${h.status !== undefined ? `<span class="status ${h.status < 400 ? "ok" : "err"}" style="min-width:50px">${h.status}</span>` : '<span style="min-width:50px;color:var(--muted)">—</span>'}
              ${h.durationMs !== undefined ? `<span class="latency ${latencyColor(h.durationMs)}">${formatDuration(h.durationMs)}</span>` : ""}
              <button class="btn btn-sm reuse-btn" style="font-size:11px;padding:2px 8px">重用</button>
            </div>`).join("")}
        </div>
      </details>
    </div>`;

  section.insertAdjacentHTML("beforeend", html);

  section.querySelectorAll(".history-row, .reuse-btn").forEach((el) => {
    el.addEventListener("click", () => {
      const row = (el as HTMLElement).closest(".history-row") as HTMLElement;
      if (!row) return;
      const p = row.getAttribute("data-path") || "/";
      const m = row.getAttribute("data-method") || "GET";
      (document.getElementById("proxy-path") as HTMLInputElement).value = p;
      (document.getElementById("proxy-method") as HTMLSelectElement).value = m;
      document.getElementById("body-section")!.style.display = ["POST", "PUT", "PATCH"].includes(m) ? "block" : "none";
    });
  });
}

// ============================================================
// 工具
// ============================================================

function escapeAttr(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
