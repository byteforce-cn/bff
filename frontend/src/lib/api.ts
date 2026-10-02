// ============================================================
// API 客户端 — 封装 BFF 管理端 API 调用
// ============================================================

// ---- 类型定义 ----

export interface RouteTypeConfig {
  upstream?: string;
  strip_prefix?: boolean;
  circuit_breaker_threshold?: number;
  proxy_mode?: "http" | "sse" | "websocket" | "auto";
}

export interface RouteDef {
  path: string;
  methods: string[];
  description: string;
  auth_required: boolean;
  type: "proxy" | "pipeline" | "script" | "static";
  config: RouteTypeConfig;
}

export interface RequestHistoryEntry {
  id: number;
  timestamp: number; // Date.now()
  path: string;
  method: string;
  status?: number;
  durationMs?: number;
  sizeBytes?: number;
}

// ---- API 函数 ----

// 管理 API 基地址。
// 业务端口（8080）不提供 /admin/api/*（会返回 404）；生产通常由网关把
// /admin/api 反代到管理端口（8443）。如需直连管理端口，可在宿主 HTML 中设置
// `window.__BFF_ADMIN_BASE__ = "http://host:8443/admin/api"`。
declare global {
  interface Window {
    __BFF_ADMIN_BASE__?: string;
  }
}
const ADMIN_BASE: string =
  (typeof window !== "undefined" && window.__BFF_ADMIN_BASE__) || "/admin/api";

/** 获取全量路由列表（后端真实路径为 /admin/api/routes；原 v2 路径不存在） */
export async function fetchRoutesV2(): Promise<RouteDef[]> {
  const resp = await fetch(`${ADMIN_BASE}/routes`);
  if (!resp.ok) throw new Error(`获取路由列表失败: HTTP ${resp.status}`);
  const data = await resp.json();
  return data.routes ?? [];
}

/** BFF 健康检查 */
export async function fetchHealth(): Promise<boolean> {
  try {
    const resp = await fetch("/api/health");
    return resp.ok;
  } catch {
    return false;
  }
}
