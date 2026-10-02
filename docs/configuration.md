# 配置参考

BFF 的配置为声明式 YAML，支持环境变量覆盖与热重载（部分项需重启，详见 [deployment.md](deployment.md) §3）。

## 文件布局与合并顺序

```text
config/
├── base.yaml            # 入口（服务端口 / 密钥 / provider / 会话 / 限流 / 安全头 ...）
├── env/
│   └── prod.yaml        # BFF_ENV=prod 时叠加（Redis provider / 严格防呆 / 持久化）
├── oidc/
│   └── providers.yaml   # OIDC Provider 列表
├── pipelines/
│   └── *.yaml           # 服务编排定义（每个文件顶层 map 合并到 pipelines 键）
├── routes/
│   └── routes.yaml      # 统一路由表（static / proxy / pipeline / script）
└── scripts/             # QuickJS 脚本（管理端创建脚本时同步落盘于此）
```

合并顺序（后者覆盖前者）：

1. `base.yaml`
2. `oidc/providers.yaml`
3. `pipelines/*.yaml`
4. `routes/routes.yaml`
5. `env/${BFF_ENV}.yaml`
6. `BFF_*` 环境变量（**最高优先级**；`__` 表示层级，如 `BFF_PROVIDER__SESSION_STORE=redis`）

> 说明：环境变量层仅解析**标量**；数组 / 对象（`oidc.providers`、`routes`、`pipelines` 等）
> 必须来自配置文件或 ConfigMap 挂载。
>
> 开启配置持久化后（`persistence.enabled`），启动加载顺序为
> `base/分文件 < runtime.yaml < BFF_* 环境变量`；`runtime.yaml` 中 `***` 哨兵按环境变量 / 基础配置回填真实值。

## 核心配置段

### server — 监听与对外地址

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `business_port` / `admin_port` | `8080` / `8443` | 业务 / 管理端口（**需重启**） |
| `public_base_url` | 空 | 对外基础 URL；设置后 `redirect_uri` / 登出回跳一律基于它推导，**不信任 Host 头** |
| `trusted_hosts` | 空 | 未配置 `public_base_url` 时的回退白名单；`prod` 强制二者至少其一 |

### bff_secret — 加密主密钥

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `secret` | `${BFF_SECRET:change-me-in-production}` | AES-256-GCM 主密钥；**生产必须注入 ≥32 字节随机值** |
| `salt` | `${BFF_SECRET_SALT:...}` | Argon2id 派生盐（≥16 字节） |

> 主密钥**不支持热更新**：导入含新密钥的配置会被显式拒绝（需重启）。

### provider — 状态组件选型

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `session_store` / `cache` / `lock` | `memory` | `memory`（单实例）或 `redis`（多实例共享）；`prod` 拒绝 `memory` |
| `redis_url` | 空 | Redis 连接串（`redis` 选型时必填） |

### session — 会话与 Cookie

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `cookie_name` | `BFF_SESSION` | 会话 Cookie 名 |
| `secure` / `http_only` | `true` | Cookie 安全属性（prod 强制 `secure`） |
| `same_site` | `Lax` | 跨站点 IdP 场景必须 `Lax`（`Strict` 会丢回调 Cookie 导致登录失败）；同站可改回 `Strict` |
| `ttl` | `14d` | 会话有效期（与 Cookie `Max-Age` 对齐） |
| `gc_interval` | `10m` | 服务端会话索引 GC 周期 |

### admin — 管理面

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `ip_whitelist` | loopback + `10.0.0.0/8` | 管理端口 IP 白名单（每请求实时读取）；生产收敛到运维网段 |
| `auth_mode` / `auth_token` | `token` / `changeme` | 管理认证；**生产必须注入 ≥32 字符随机口令**（prod 强制） |
| `trusted_proxies` | `0` | XFF 可信代理跳数（按入口拓扑标定） |
| `auth_fail_limit_per_minute` | `30` | 认证失败限流（按来源 IP） |
| `enable_test_endpoints` | `true`（prod 强制 `false`） | 脚本 eval / pipeline 试跑等测试端点开关 |
| `max_body_bytes` | `8 MiB` | 管理 API 请求体上限 |

### spa — 静态资源

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `dir` | `frontend/dist` | 业务端口托管的 SPA 目录（运行时资源，未构建时 SPA 路径 404，不影响 API） |

### http_client — 出网客户端（代理 / 编排）

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `connect_timeout` / `timeout` | `5s` / `30s` | 连接 / 总超时；SSE 走独立无总超时客户端 |
| `tcp_keepalive` | `60s` | TCP keepalive |
| `retry_max_attempts` / `retry_backoff` | `0` / `100ms` | 代理重试（默认仅幂等请求） |
| `max_concurrent_per_upstream` | `0` | 每上游并发上限（0 = 不限制） |
| `client_cert_path` / `client_key_path` / `ca_cert_path` | 空 | mTLS / 自定义 CA（内网自签场景） |

### rate_limit — 全局限流

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `per_second` / `burst_size` | `50` / `500` | 按真实客户端 IP 建桶；`per_second` = 持续速率（每秒补液） |
| `skip_path_prefixes` | `/assets/`、`/favicon.ico`、`/index.html` | 静态资源不消耗令牌 |

> ⚠️ 底层 `tower-governor` 0.4.x 的 `per_second` 为「每 N 秒 1 个」的周期语义，
> 实现已显式换算并有回归测试；新增配置项勿直接透传（见 `src/middleware/rate_limit_skip.rs`）。

### auth_rate_limit — 认证端点 per-IP 限流

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `enabled` / `trusted_proxies` | `false` / `1` | 生产模板已启用；跳数按入口拓扑标定 |
| `per_ip.per_second` / `burst_size` | `5` / `20` | 令牌桶参数 |
| `paths` | `/oauth2/authorize` 等 | 命中的路径前缀 |

### cors / security_headers / body_limit

| 段 | 关键字段 | 说明 |
| --- | --- | --- |
| `cors` | `permissive` / `allowed_origins` | 默认不允许跨域；显式白名单才放行 |
| `security_headers` | `content_security_policy`（+ `csp_overrides` 按路径）、`x_frame_options`、`hsts_max_age`、`referrer_policy` | HSTS 默认 0（由 LB 处理 TLS 时可配置下发） |
| `body_limit` | `max_bytes`（10 MiB）、`max_response_bytes`（64 MiB） | 统一作用于代理 / 编排 / 脚本 / 管理面 |

### circuit_breaker / websocket / scripting / health

| 段 | 关键字段 | 说明 |
| --- | --- | --- |
| `circuit_breaker` | `failure_threshold: 5`、`failure_window: 60s`、`open_duration: 30s` | 滚动窗口失败计数 + 半开单探针 |
| `websocket` | `connect_timeout: 5s`、`idle_timeout: 300s`、`heartbeat_interval: 30s`、`max_message_bytes: 1 MiB` | 隧道生命周期与上限 |
| `scripting` | `max_duration: 2s` | QuickJS 执行时长上限 |
| `health` | `upstreams`（空则从 routes 推导）、`probe_timeout: 2s`、`allow_degraded`、`probe_path` | `/ready` 探针行为 |

### persistence — 配置持久化（可选）

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `enabled` | `false`（prod 模板 `true`） | 管理端变更落盘；多副本共享同一文件实现收敛 |
| `path` | `runtime.yaml`（容器内 `/data/bff/runtime.yaml`） | 需为可写持久化卷（K8s `fsGroup: 10001`） |
| `watch_interval` | `5s` | 外部变更轮询周期 |

### telemetry — 分布式追踪（可选）

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `otlp_endpoint` | 空（完全禁用） | OTLP/gRPC 端点（`https` 走 rustls） |
| `service_name` / `sample_ratio` | `bff` / `1.0` | 服务名与**根请求**采样率（含上游上下文时跟随上游采样位） |

### oidc.providers / token_refresh / routes / pipelines

| 段 | 说明 |
| --- | --- |
| `oidc.providers` | Provider 列表：`id` / `issuer_url` / `client_id` / `client_secret` / `scopes` / `callback_path` 等；支持管理端动态维护（热生效） |
| `token_refresh.skip_prefixes` | 跳过令牌刷新检查的路径前缀（登录 / 回调 / 静态资源等） |
| `routes`（`routes/routes.yaml`） | 统一路由表；字段与行为见 [architecture.md](architecture.md)「统一路由分发」 |
| `pipelines`（`pipelines/*.yaml`） | 服务编排 DAG 定义 |

## 环境变量

- 任意字段可用 `BFF_` 前缀 + `__` 分层覆盖：`BFF_SERVER__PUBLIC_BASE_URL`、`BFF_PROVIDER__SESSION_STORE` 等；
- `BFF_ENV` 选择叠加的环境文件（如 `prod` → `config/env/prod.yaml`）；
- `BFF_SECRET` / `BFF_SECRET_SALT` 是主密钥的专用注入变量（默认值仅为本地占位）。

## 生产防呆（`BFF_ENV=prod`）

启动时强制校验，不满足即**拒绝启动**：

- 管理口令弱（<32 字符）或 `auth_mode=none`；
- `provider.* = memory`（无法多实例）；
- `enable_test_endpoints=true`；
- `insecure_skip_id_token_verification=true`；
- `session.secure=false`；
- `bff_secret` 仍为默认弱值；
- 未配置 `public_base_url` 且 `trusted_hosts` 为空。

## 热生效边界

热生效项（每请求读取）与需重启项（启动构建）的完整对照表见 [deployment.md](deployment.md) §3；
运维口径：管理端改完配置后先验证热生效项，涉及需重启项走滚动发布。
