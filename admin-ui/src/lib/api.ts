// ============================================================
// API 客户端：封装所有 BFF 管理 API 调用
// ============================================================

const BASE = "/admin/api";

function getToken(): string {
  return localStorage.getItem("bff_admin_token") || "";
}

async function request<T = unknown>(
  path: string,
  options: RequestInit = {}
): Promise<T> {
  const headers: Record<string, string> = {
    "X-Admin-Token": getToken(),
    ...((options.headers as Record<string, string>) || {}),
  };

  const resp = await fetch(`${BASE}${path}`, { ...options, headers });

  if (resp.status === 401) {
    localStorage.removeItem("bff_admin_token");
    window.location.href = "/login";
    throw new Error("未授权，请重新登录");
  }

  if (!resp.ok) {
    const body = await resp.text();
    let msg = `${resp.status} ${resp.statusText}`;
    try {
      const err = JSON.parse(body);
      msg = err.error || msg;
    } catch {
      msg = body || msg;
    }
    throw new Error(msg);
  }

  const ct = resp.headers.get("content-type") || "";
  if (ct.includes("application/json")) {
    return resp.json();
  }
  return resp.text() as unknown as T;
}

// ---- 认证 ----
export async function verifyToken(): Promise<boolean> {
  try {
    await request("/health");
    return true;
  } catch {
    return false;
  }
}

// ---- 健康 / 指标 / 会话 ----
export const health = () => request("/health");
export const metrics = () => request<string>("/metrics");
export const listSessions = () => request("/sessions");
export const revokeSession = (id: string) =>
  request(`/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });

// ---- 配置 ----
export const exportConfig = () => request<string>("/config/export");
export const importConfig = (yaml: string) =>
  request("/config/import", {
    method: "POST",
    headers: { "Content-Type": "application/yaml" },
    body: yaml,
  });

// ---- OIDC Providers ----
export const listProviders = () => request("/oidc/providers");
export const updateProvider = (id: string, provider: Record<string, unknown>) =>
  request(`/oidc/providers/${encodeURIComponent(id)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(provider),
  });

// ---- Pipelines ----
export const listPipelines = () => request("/pipelines");
export const createPipeline = (name: string, def: Record<string, unknown>) =>
  request(`/pipelines?name=${encodeURIComponent(name)}`, {
    method: "POST",
    headers: { "Content-Type": "application/yaml" },
    body: def as unknown as string, // send raw body
  });
export const deletePipeline = (name: string) =>
  request(`/pipelines/${encodeURIComponent(name)}`, { method: "DELETE" });

// ---- Scripts ----
export const listScripts = () => request("/scripts");
export const updateScript = (name: string, content: string) =>
  request(`/scripts/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "text/plain" },
    body: content,
  });
export const evalScript = (
  name: string,
  script: string,
  inputs: Record<string, unknown> = {},
  session?: Record<string, unknown>,
  env?: Record<string, unknown>
) =>
  request(`/scripts/${encodeURIComponent(name)}/eval`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ script, inputs, session, env }),
  });

// ---- Pipeline Test ----
export interface PipelineTestParams {
  params?: Record<string, string>;
  session?: Record<string, unknown>;
  env?: Record<string, unknown>;
  dry_run?: boolean;
  timeout_override?: string;
}

export const testPipeline = (name: string, opts: PipelineTestParams = {}) =>
  request(`/pipelines/${encodeURIComponent(name)}/test`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(opts),
  });

// ---- Routes（统一路由） ----
export const listRoutes = () => request("/routes");
export const updateRoutes = (routes: Record<string, unknown>[]) =>
  request("/routes", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(routes),
  });
export const listRouteTypes = () => request("/routes/types");
