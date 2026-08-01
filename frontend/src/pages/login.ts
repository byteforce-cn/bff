// ============================================================
// 登录页：OIDC 登录流程测试
// ============================================================

import { escapeHtml } from "../lib/utils";

export function renderLogin(el: HTMLElement) {
  el.innerHTML = `
    <h1 style="margin-bottom:24px">🔐 OIDC 登录</h1>

    <div class="card">
      <h2>登录状态</h2>
      <div id="login-status"><span class="status loading">检测中…</span></div>
    </div>

    <div class="card">
      <h2>🧪 测试说明</h2>
      <p style="font-size:13px;color:var(--muted)">
        登录流程：<br/>
        1. 前端跳转到 <code>/login</code>（BFF 内置端点）<br/>
        2. BFF 重定向到 OIDC Provider 授权页<br/>
        3. 用户授权后，Provider 回调 <code>/auth/callback</code><br/>
        4. BFF 完成令牌交换，设置 Session Cookie<br/>
        5. 重定向回 SPA —— 此时已登录
      </p>
    </div>
  `;

  checkLoginStatus();
}

async function checkLoginStatus() {
  const el = document.getElementById("login-status")!;
  try {
    // 通过访问需认证的路由判断登录状态
    const resp = await fetch("/api/dashboard");
    const isLoggedIn = resp.status !== 401 && resp.status !== 403;

    if (isLoggedIn) {
      el.innerHTML = `
        <h2>✅ 已登录</h2>
        <p style="color:var(--muted);margin-bottom:16px">
          你已通过 OIDC 认证。访问 <code>/api/dashboard</code> 返回 ${resp.status}。
        </p>
        <div style="display:flex;gap:8px">
          <a class="btn btn-danger" href="/logout">退出登录</a>
          <a class="btn btn-primary" href="#/dashboard">📊 查看仪表盘</a>
          <a class="btn" href="#/">返回首页</a>
        </div>
      `;
    } else {
      el.innerHTML = `
        <p style="color:var(--muted);margin-bottom:16px">
          当前未登录。点击下方按钮启动 OIDC 认证流程。
        </p>
        <a class="btn btn-primary" href="/login">
          🚀 前往 OIDC Provider 登录
        </a>

        <div class="card" style="margin-top:16px;border-left:3px solid #f59e0b">
          <h2>⚠️ Provider 连通性探测</h2>
          <div id="provider-probe"></div>
        </div>
      `;
      probeProvider();
    }
  } catch {
    el.innerHTML = `<span class="status err">无法连接 BFF</span>`;
  }
}

async function probeProvider() {
  const el = document.getElementById("provider-probe")!;
  el.innerHTML = `<span class="status loading">探测 /login 重定向…</span>`;

  try {
    const resp = await fetch("/login", { redirect: "manual" });
    if (resp.type === "opaqueredirect" || (resp.status >= 300 && resp.status < 400)) {
      el.innerHTML = `<span class="status ok">/login 返回重定向 (${resp.status}) — Provider 已配置</span>`;
    } else {
      const text = await resp.text();
      el.innerHTML = `
        <span class="status" style="color:#f59e0b">/login 返回 ${resp.status}</span>
        <pre style="margin-top:8px;max-height:120px;font-size:12px">${escapeHtml(text.slice(0, 500))}</pre>`;
    }
  } catch {
    el.innerHTML = `<span class="status err">无法连接 BFF</span>`;
  }
}
