// ============================================================
// 类型定义：与 BFF 后端 API 返回结构对齐
// ============================================================

/** OIDC Provider 配置 */
export interface OidcProviderConfig {
  id: string;
  display_name: string;
  issuer_url: string;
  client_id: string;
  client_secret: string;
  callback_path: string;
  scopes: string[];
  insecure_skip_id_token_verification: boolean;
  refresh_skew_secs: number;
}

/** Pipeline 编排定义 */
export interface PipelineDef {
  strategy: StrategyDef;
  steps: StepDef[];
}

export interface StrategyDef {
  timeout: string; // humantime 格式，如 "10s"
  error_handling: "fail_fast" | "continue";
}

export interface StepDef {
  id: string;
  type: "http_request" | "script";
  depends_on: string[];
  config: StepConfig;
}

export interface StepConfig {
  url?: string;
  method: string;
  timeout?: string;
  cache_ttl?: string;
  headers: Record<string, string>;
  body?: string;
  script?: string;
}

// ============================================================
// 统一路由类型
// ============================================================

/** 统一路由定义 */
export interface RouteDef {
  path: string;
  methods: string[];
  description: string;
  auth_required: boolean;
  type: "proxy" | "pipeline" | "script" | "static";
  config: RouteTypeConfig;
  input_mapping: InputMapping;
  output_mapping: OutputMapping;
}

export interface RouteTypeConfig {
  // Proxy 专属
  upstream?: string;
  strip_prefix?: boolean;
  circuit_breaker_threshold?: number;
  // Pipeline 专属
  pipeline?: string;
  pipeline_inline?: PipelineDef;
  // Script 专属
  script?: string;
  script_inline?: string;
  // Static 专属
  status?: number;
  body?: unknown;
  headers?: Record<string, string>;
}

export interface InputMapping {
  from_query: Record<string, string>;
  from_body: Record<string, string>;
  from_path: Record<string, string>;
  from_header: Record<string, string>;
  from_session: Record<string, string>;
  from_env: Record<string, string>;
  defaults: Record<string, unknown>;
}

export interface OutputMapping {
  wrap?: string;
  status_map: Record<string, number>;
  rename: Record<string, string>;
  pick: string[];
}

/** Session 信息 */
export interface SessionInfo {
  id: string;
  created_at: string;
  expires_at: string;
  [key: string]: unknown;
}

/** 完整应用配置 */
export interface AppConfig {
  server: {
    business_port: number;
    admin_port: number;
  };
  provider: {
    session_store: "memory" | "redis";
    cache: "memory" | "redis";
    lock: "memory" | "redis";
    redis_url: string;
  };
  session: {
    cookie_name: string;
    secure: boolean;
    http_only: boolean;
    same_site: string;
  };
  admin: {
    ip_whitelist: string[];
    auth_mode: "token" | "none";
    auth_token: string;
  };
  spa: {
    dir: string;
  };
  oidc: {
    providers: OidcProviderConfig[];
  };
  pipelines: Record<string, PipelineDef>;
  routes: RouteDef[];
}

/** API 列表响应 */
export interface ListResponse<T> {
  [key: string]: T;
}
