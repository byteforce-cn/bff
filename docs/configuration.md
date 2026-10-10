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
| `cookie_name` | `BFF_SESSION` | 会话 Cookie 名；跨全部 profile（含 `default`）**全局唯一** |
| `cookie_domain` | 空 | 非空 = Domain cookie（`.example.com` / `example.com` 均接受并归一）；缺省/空串 = host-only；**需重启** |
| `secure` / `http_only` | `true` | Cookie 安全属性（prod 强制 `secure`） |
| `same_site` | `Lax` | `Strict` / `Lax` / `None`（YAML 字符串）；跨站点 IdP 顶层导航回调必须 `Lax`（`Strict` 会丢回调 Cookie 导致登录失败），同站可改回 `Strict`；`"None"` 必须带引号且强制 `secure: true` |
| `ttl` | `14d` | 会话有效期（与 Cookie `Max-Age` 对齐） |
| `gc_interval` | `10m` | 服务端会话索引 GC 周期（**进程级**，仅顶层生效） |
| `allow_unmanaged_subdomains` | `false` | 共享域信任边界显式确认：`cookie_domain` 非空时 prod 必须为 `true`（否则拒绝启动），dev 仅告警（见 §多站点启动期校验） |

### sites — 多站点定义

`sites` 非空即进入**显式多站点**模式：每个站点一个监听端口与 router；缺省（为空）时按 §5.5
合成名为 `default` 的 legacy 单站点，行为与升级前完全一致。

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `name` | 必填 | 站点名，唯一，匹配 `[a-z0-9-]+`；保留名 `admin` 禁止；用作指标 / 日志 / 会话命名空间 |
| `port` | 必填 | 监听端口；唯一、非 `0`、≠ `server.admin_port`（**需重启**） |
| `bind` | `0.0.0.0` | 监听地址（**需重启**） |
| `server_names` | 空 | Host 白名单条目（**仅主机名**，拒绝 `://` / `/` / 端口 / 通配符 `*`）；有效白名单 = `server_names ∪ {public_base_url 主机}`（热生效） |
| `public_base_url` | 空（prod 必填） | 对外基础 URL（http/https、无 path/query）；`redirect_uri` / 登出回跳一律由此推导、**不信任 Host**（热生效） |
| `session_profile` | `default` | 引用的会话 profile（见下；**需重启**） |
| `spa` | 继承顶层 `spa` | 站点级 SPA 目录 `spa.dir`（热生效） |
| `oidc.default_provider` | 空 | 站点默认 provider（引用 `oidc.providers[].id`） |
| `oidc.allowed_providers` | `[default_provider]` | 站点可用 provider 白名单；`?provider=` 越站返回 **400** 且不回退默认 |
| `logout_scope` | `global` | `global` = 清全站 token（`session.flush()`）+ RP-Initiated Logout；`site` = 仅清本站点 token、**不触发 IdP 登出**（热生效） |
| `security_headers` | 无 | Partial 覆盖（见「安全响应头」），未指定字段继承全局 |

> 显式多站点下顶层 `server.public_base_url` / `server.trusted_hosts` 为 **dead config**（非空即启动失败）；
> `server.business_port` 被忽略（≠ 8080 时仅告警）。

### session_profiles — 会话 profile 覆盖

顶层 `session` 即 profile `default` 的定义（零改动兼容）；额外 profile 通过 `session_profiles`
声明，**未指定字段继承顶层 `session` 后覆盖**。同 profile 内站点共享一份 `SessionStore`；
跨子域共享会话要求该 profile 的 `cookie_domain` 非空。`session_profiles` 不得定义 `default` 键，
profile 名匹配 `[a-z0-9-]+`。

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `cookie_name` | 继承顶层 | 会话 Cookie 名；跨全部 profile（含 `default`）**全局唯一**（**需重启**） |
| `cookie_domain` | 继承顶层 | 非空 = Domain cookie；显式空串 `""` = 强制 host-only（用于隔离组）（**需重启**） |
| `secure` / `http_only` | 继承顶层 | Cookie 安全属性（prod 逐 profile 校验 `secure`）（**需重启**） |
| `same_site` | 继承顶层 | `Strict` / `Lax` / `None`；`"None"` 必须带引号且强制 `secure: true`（**需重启**） |
| `ttl` | 继承顶层 | 会话有效期（profile 级）（**需重启**） |
| `allow_unmanaged_subdomains` | `false` | 共享域信任边界显式确认（prod 下 `cookie_domain` 非空时必须 `true`） |

### security_headers — 站点级安全响应头覆盖

`sites[].security_headers` 为 **Partial 类型**（字段全部可选）：未指定字段继承全局
`security_headers`；`content_security_policy` 一旦指定即**整体替换**（含全局按路径的
`csp_overrides`）。可覆盖字段：`content_security_policy`、`x_frame_options`、
`x_content_type_options`、`hsts_max_age`、`referrer_policy`。安全响应头在配置替换时
**预构建为 `HeaderMap`**，请求路径零解析开销（热生效）。

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
| `oidc.providers` | Provider 列表：`id` / `issuer_url` / `client_id` / `client_secret` / `scopes` / `callback_path` / `shared_across_sites` 等；支持管理端动态维护（热生效，`callback_path` 集合变化除外） |
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
- 未配置 `public_base_url` 且 `trusted_hosts` 为空；
- **多站点**：显式多站点下某站点缺 `public_base_url`、`cookie_domain` 非空却未
  `allow_unmanaged_subdomains: true`、同一 profile 内站点共享 provider 却未
  `shared_across_sites: true` 等（详见下节）。

## 多站点启动期校验（fail-fast）

显式多站点（`sites` 非空）在启动时逐条校验，任一失败即**拒绝启动**；错误信息含字段路径
（如 `sites[0].port`、`session_profiles[isolated].cookie_name`）：

1. 站点 `name` 非空、唯一、匹配 `[a-z0-9-]+`；保留名 `admin` 禁止（error）；
2. `port` 唯一、非 `0`、≠ `admin_port`；`bind` 为可解析 IP；
3. `session_profile` 引用存在；profile 名合法且不得为 `default`；`cookie_name` 全局唯一；
   `same_site ∈ {Strict, Lax, None}` 且 `None ⇒ secure: true`（prod 逐 profile 校验 `secure`）；
4. `cookie_domain` 非空时须正向 domain-match 引用该 profile 的全部站点主机；被 ≥2 个不同主机站点
   引用且 `cookie_domain` 为空 → prod 启动失败、dev 告警；
5. prod 下 `cookie_domain` 非空且未显式 `allow_unmanaged_subdomains: true` → 启动失败；
6. prod 下每站点必须配置 `public_base_url`；`server_names` 若配置须包含 public 主机；站点间
   归一化后 `public_base_url` 唯一；顶层 `server.public_base_url` / `server.trusted_hosts` 非空 → error；
   `server.business_port` 被忽略（≠ 8080 时 warn）；
7. `server_names` 条目仅主机名：拒绝 `://` / `/` / 端口 / 通配符 `*`；大小写不敏感、去尾点、站点内唯一；
8. 站点绑定的 provider 必须存在；`default_provider ∈ allowed_providers`；**同一站点**绑定的 provider
   `callback_path` 必须唯一（error）。**legacy 豁免**：无 `sites` 时合成站点绑定全部 provider，
   多 provider 共享 `callback_path` 仍按 `?provider=` 选择，既有配置原样启动（§5.4 第 12 条）；
9. `routes[].sites` 引用的站点必须已定义（仅显式多站点校验）；
10. 同一 profile 内被 >1 个站点引用的 provider 必须显式 `oidc.providers[].shared_across_sites: true`，
    否则启动失败（保证站点间令牌互不可见）；
11. prod 下多站点解析到同一 `spa.dir` → warn；
12. `sites` 为空 ⇔ legacy 模式：保持既有全局校验、多 provider 选择语义（无 `?provider=` → **400**）
    与 Host 校验路径完全不变。

## 热生效边界

多站点模式的 `sites[].server_names` / `public_base_url` / `spa.dir` / `oidc` 绑定 /
`security_headers` / `logout_scope` 为**热生效**（每请求从配置快照按站点名解析）；
`sites[].port` / `bind` / `session_profile`、profile 解析后的 cookie 策略
（`cookie_name` / `cookie_domain` / `secure` / `http_only` / `same_site` / `ttl`）等属**需重启**
（结构指纹）。热生效项（每请求读取）与需重启项（启动构建）的完整对照表见
[deployment.md](deployment.md) §3；运维口径：管理端改完配置后先验证热生效项，涉及需重启项走滚动发布。
