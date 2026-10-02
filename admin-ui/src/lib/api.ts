// ============================================================
// API 客户端：封装所有 BFF 管理 API 调用
// ============================================================

const BASE = "/admin/api";

// 管理 token 存 sessionStorage（关标签页即失效）而非 localStorage，
// 降低持久化窃取面；cookie/localStorage 都不可用时退化为仅内存（刷新需重登）。
const TOKEN_KEY = "bff_admin_token";
let memoryToken = "";

export function getToken(): string {
  if (memoryToken) return memoryToken;
  try {
    return sessionStorage.getItem(TOKEN_KEY) || "";
  } catch {
    return "";
  }
}

export function setToken(token: string): void {
  memoryToken = token;
  try {
    sessionStorage.setItem(TOKEN_KEY, token);
  } catch {
    /* 存储受限：仅内存持有 */
  }
}

export function clearToken(): void {
  memoryToken = "";
  try {
    sessionStorage.removeItem(TOKEN_KEY);
  } catch {
    /* ignore */
  }
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
    clearToken();
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

/** 真实删除 provider（后端 DELETE 端点） */
export const deleteProvider = (id: string) =>
  request(`/oidc/providers/${encodeURIComponent(id)}`, { method: "DELETE" });

/** 真实连通性校验（后端执行 OIDC discovery） */
export interface ProviderVerifyResult {
  ok: boolean;
  issuer?: string;
  token_endpoint?: string;
  jwks_uri?: string;
  latency_ms?: number;
  error?: string;
}
export const verifyProvider = (id: string) =>
  request<ProviderVerifyResult>(`/oidc/providers/${encodeURIComponent(id)}/verify`, {
    method: "POST",
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
