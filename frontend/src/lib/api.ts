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

/** 获取全量路由列表 (v2) */
export async function fetchRoutesV2(): Promise<RouteDef[]> {
  const resp = await fetch("/admin/api/routes/v2");
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
