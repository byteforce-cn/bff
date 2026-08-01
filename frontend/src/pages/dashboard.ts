// ============================================================
// 仪表盘：Pipeline 编排测试 (/api/echo, /api/dashboard)
// ============================================================

import { escapeHtml, formatDuration, formatBytes, formatError, latencyColor } from "../lib/utils";

interface PipelineDef {
  path: string;
  label: string;
  auth: boolean;
  desc: string;
  inputMapping?: string;
  outputMapping?: string;
}

const PIPELINES: PipelineDef[] = [
  {
    path: "/api/echo",
    label: "echo",
    auth: false,
    desc: "快速验证 pipeline 引擎可用性，无需认证。",
  },
  {
    path: "/api/dashboard",
    label: "dashboard",
    auth: true,
    desc: "聚合用户信息 + 订单列表。userId 从 OIDC session 的 sub 字段注入。",
    inputMapping: "from_session: { userId: sub }",
    outputMapping: 'pick: ["user", "orders", "generated_at"], wrap: data',
  },
];

export function renderDashboard(el: HTMLElement) {
  el.innerHTML = `
    <h1 style="margin-bottom:24px">📊 Pipeline 编排测试</h1>

    <div class="card">
      <h2>📋 已注册 Pipeline 路由</h2>
      <table>
        <thead><tr>
          <th>路由</th><th>Pipeline</th><th>认证</th><th>说明</th><th>操作</th>
        </tr></thead>
        <tbody>${PIPELINES.map((p, i) => `
          <tr>
            <td><code style="font-size:12px">GET ${escapeHtml(p.path)}</code></td>
            <td>${escapeHtml(p.label)}</td>
            <td>${p.auth ? '<span style="color:var(--success)">✓</span>' : '<span style="color:var(--muted)">—</span>'}</td>
            <td style="font-size:12px;color:var(--muted)">
              ${escapeHtml(p.desc)}
              ${p.inputMapping ? `<br/><span style="font-size:11px">📥 ${escapeHtml(p.inputMapping)}</span>` : ""}
              ${p.outputMapping ? `<br/><span style="font-size:11px">📤 ${escapeHtml(p.outputMapping)}</span>` : ""}
            </td>
            <td><button class="btn btn-primary btn-sm exec-btn" data-idx="${i}">▶ 执行</button></td>
          </tr>`).join("")}
        </tbody>
      </table>
    </div>

    <div class="card">
      <h2>📤 执行结果</h2>
      <div id="pipeline-result">
        <p style="color:var(--muted);font-size:13px">点击表格中的"执行"按钮…</p>
      </div>
    </div>

    <div class="card">
      <h2>🧪 测试说明</h2>
      <p style="font-size:13px;color:var(--muted)">
        Pipeline 定义在 <code>config/pipelines/*.yaml</code> 中。<br/>
        BFF 根据 DAG 依赖关系解析步骤执行顺序，支持 HTTP 请求和 Rhai 脚本。<br/>
        <code>echo</code> 用于快速验证引擎可用，<code>dashboard</code> 演示并行 HTTP 调用 + 脚本合并。<br/>
        路由注册在 <code>config/routes/routes.yaml</code> 的 <code>routes_v2</code> 段。
      </p>
    </div>
  `;

  // 绑定所有执行按钮
  el.querySelectorAll(".exec-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const idx = parseInt((btn as HTMLElement).dataset.idx || "0", 10);
      runPipeline(PIPELINES[idx]);
    });
  });
}

async function runPipeline(def: PipelineDef) {
  const resultEl = document.getElementById("pipeline-result")!;
  resultEl.innerHTML = `<span class="status loading">GET ${escapeHtml(def.path)} …</span>`;

  const start = performance.now();
  try {
    const resp = await fetch(def.path);
    const elapsed = performance.now() - start;
    const body = await resp.text();
    const sizeBytes = new Blob([body]).size;

    let formatted: string;
    try {
      formatted = JSON.stringify(JSON.parse(body), null, 2);
    } catch {
      formatted = body;
    }

    const statusClass = resp.ok ? "ok" : "err";
    const colorClass = latencyColor(elapsed);

    // 解析响应头
    const respHeaders: Record<string, string> = {};
    resp.headers.forEach((v, k) => { respHeaders[k] = v; });

    resultEl.innerHTML = `
      <div style="display:flex;align-items:center;gap:12px;margin-bottom:8px;flex-wrap:wrap">
        <span class="status ${statusClass}">HTTP ${resp.status}</span>
        <span class="latency ${colorClass}">⏱ ${formatDuration(elapsed)}</span>
        <span style="font-size:12px;color:var(--muted)">📦 ${formatBytes(sizeBytes)}</span>
        <code style="font-size:12px;color:var(--muted)">${escapeHtml(def.path)}</code>
      </div>
      <details style="margin-bottom:8px">
        <summary style="cursor:pointer;font-size:12px;color:var(--muted)">响应头</summary>
        <pre style="margin-top:4px;max-height:120px;font-size:12px">${escapeHtml(JSON.stringify(respHeaders, null, 2))}</pre>
      </details>
      <details open>
        <summary style="cursor:pointer;font-size:12px;color:var(--muted)">响应体</summary>
        <pre style="margin-top:4px;max-height:400px;font-size:12px">${escapeHtml(formatted)}</pre>
      </details>`;
  } catch (e) {
    resultEl.innerHTML = `
      <span class="status err">请求失败</span>
      <pre style="margin-top:8px;font-size:12px">${escapeHtml(formatError(e))}</pre>`;
  }
}
