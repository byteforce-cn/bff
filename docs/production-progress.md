# BFF 生产落地进度记录（Production Progress）

> 依据：`docs/production-readiness.md`（审计 v2，基线 `223fd3c`）。本文件记录实际实施与验证证据。
> 编号（P0-x / Sx / Rx / Ex / Fx）与审计报告一一对应。

| 项目 | 内容 |
| ---- | ---- |
| 记录日期 | 2026-09-27（第三轮：Keycloak 真实 IdP 契约验证 + 3 个真实缺陷修复） |
| 实施阶段 | **P0 全部关闭**；M2 安全主体完成；M3 观测/运营资产完成；真实 IdP 兼容性已验证；M4 灰度待环境 |
| 门禁状态 | `cargo fmt` ✅ / `cargo clippy -D warnings` ✅ / `cargo test` ✅（**157 passed / 0 failed**，另有 2 个依赖 fakesvc 的用例 ignored） |
| 新增测试 | 第三轮：scope 去重单测（handlers）+ 会话索引回归断言（test_oidc_flow）；累计含既有全量回归 |

---

## 1. 已完成项（含验证证据）

### M0-1  CI 门禁（E1 / E13）✅

- `.github/workflows/ci.yml`：Rust job 增加 **pnpm + admin-ui 构建前置**（`RustEmbed` 编译期强依赖 `admin-ui/dist`，干净检出不再编译失败）；**删除全局 `RUSTFLAGS: -D warnings`**，`-D warnings` 仅保留在 clippy 步骤（避免 `cargo test` 被 rustc 警告打死）。
- 验证：`cargo fmt --all -- --check` 退出 0；`cargo clippy --all-targets --all-features -- -D warnings` 退出 0；`cargo test --all-features` 全绿。

### M0-2  门禁修复（E2 / E3 / E12）✅

| 项 | 修复 | 验证 |
| --- | --- | --- |
| E2 fmt | `cargo fmt`（原 diff：`src/scripting/mod.rs:68`） | fmt check 通过 |
| E2 clippy | 修复 15(lib)+5(test) 处：deprecated `GenericArray::from_slice`→`Key::from`/数组转换、未用导入/变量、`needless_return`、`derivable_impls`、`needless_late_init`、`DoubleEndedIterator::last`、`io::Error::other`、`too_many_arguments`（显式 allow）等 | clippy `-D warnings` 通过 |
| E3 时序测试 | `tests/test_ip_rate_limit.rs::test_auth_rate_limit_refill_after_wait` 改为 **rate=1/s + burst=5**（净消耗 ≈1−d，桶必然见底，不依赖“27ms/60ms”时延假设）；`!= 429` 弱断言升级为确定状态码（400/200） | 用例稳定通过 |
| E12 代理敏感 | `tests/common/mod.rs::test_client()` 增加 `.no_proxy()`；本机确认无 `HTTP_PROXY` | 全量测试稳定 |

### M0-3  P0-5：`/pipeline/:name` 匿名执行入口 ✅

- `src/server/business.rs::run_pipeline` 注入 `Session` 并强制鉴权（无有效会话 → 401，且**先鉴权后存在性检查**，不泄露 pipeline 清单）。需要匿名执行的 pipeline 应走 `routes.yaml` 的 `auth_required: false` 统一分发路径。
- 回归测试 `tests/test_pipeline_auth.rs`（3 例）：匿名 GET/POST → 401、匿名探测不存在 pipeline 仍 401。
- 既有测试适配：`test_orchestration` / `test_script_engine` / `test_admin_config_import_export` 改携带 `common::login_cookie()`。

### M0-4  P0-3：密钥管理 ✅

- `AppConfig::sanitized()` 扩展覆盖：**`bff_secret.{secret,salt}`**、含凭据的 `provider.redis_url`、`http_client.client_key_path`、OIDC `client_secret`、`admin.auth_token`、`token_exchange.client_secret`。
- `merge_sensitive_secrets()` 对称回填上述全部字段（OIDC 按 `id`、token_exchange 按 `route.path` 对齐）。
- **`bff_secret` 热导入显式拒绝**：`AppState::replace_config` 检测到密钥变化即报错（提示重启），消除“配置显示新密钥、实际用旧密钥”的静默分裂（原 `crypto::init` 进程级 `OnceLock` 特性）。
- `update_provider` / `update_routes` 增加 `***` 哨兵语义（保留现网密钥，不再把哨兵写回真实配置）。
- 测试：
  - `src/config.rs` 单测 ×3：脱敏无泄露 / 导出→回导完整恢复 / 非哨兵值不被覆盖；
  - `tests/test_admin_config_import_export.rs`：导出断言不含 `BFF_SECRET` 明文；回导后原管理口令仍有效；热导入 `bff_secret` → 422。

### M0-5  生产配置防呆（validate 强化）✅

`BFF_ENV=prod` 时 `AppConfig::validate()` 强制拒绝：

- `provider.*=memory`（无法多实例、重启丢状态）；
- `admin.auth_mode=none`；`admin.auth_token` 弱口令/长度 <32；
- `admin.enable_test_endpoints=true`（`prod.yaml` 已改为 false）；
- OIDC `insecure_skip_id_token_verification=true`；
- `session.secure=false`；
- `bff_secret` 仍为默认弱值（要求 `BFF_SECRET`/`BFF_SECRET_SALT` 注入）；
- 未配置 `server.public_base_url` 且 `trusted_hosts` 为空（防 Host 污染）。

实测：

```text
# 拒绝（默认 POC 配置）
BFF_ENV=prod ./target/debug/bff
→ Error: 生产环境（BFF_ENV=prod）admin.auth_token 必须为 ≥32 字符的随机值...

# 通过（强口令 + Redis 可达）
BFF_ENV=prod BFF_PROVIDER__REDIS_URL=redis://127.0.0.1:6379 \
  BFF_ADMIN__AUTH_TOKEN=<32+字节> BFF_SECRET=<...> BFF_SECRET_SALT=<...> ./target/debug/bff
→ 正常监听 8080/8443，SIGTERM 后优雅退出（EXIT=0）
```

### M0-6  F14：配置优先级修正 ✅

- `AppConfig::load` 合并顺序改为：base → providers → pipelines → routes → `env/{BFF_ENV}.yaml` → **`BFF_*` 环境变量（最高，12-factor）**。
- 验证：`BFF_ENV=prod BFF_PROVIDER__SESSION_STORE=memory ...` 现在环境变量能覆盖文件并触发防呆（修复前会被 prod.yaml 反向覆盖）。

### M0-7  容器化交付（P0-2 交付物部分）✅（镜像与 compose 已实测）

- `Dockerfile`：4 阶段（admin-ui 构建 → frontend 构建 → Rust release（含依赖缓存）→ debian-slim 最小运行镜像，数值 UID、无 apt 运行时依赖）。
- `.dockerignore`；`docker-compose.yml`（`bff-redis` 服务名与 `prod.yaml` 的 `redis_url` 对齐；`BFF_ENV=prod` 全量防呆；`BFF_ADMIN_TOKEN` 等**无默认值**，缺失即报错；`BFF_SERVER__PUBLIC_BASE_URL` 满足生产防呆）。
- 实测（2026-09-27）：
  - `docker build -t bff:local .` ✅（镜像 142MB；构建过程修复两处：运行阶段去除 apt 依赖、pnpm `--ignore-scripts` 适配新版构建脚本策略）；
  - `docker compose up -d` → 两个容器 Up（redis `healthy`）；
  - `/live` = 200；管理 `/admin/api/v1/health` = 200（token + IP 白名单生效）；
  - `config/export` 中 `client_secret` / `bff_secret.secret` / `salt` 全部为 `***`，真实 `BFF_SECRET` 出现次数 = **0**；
  - `docker compose stop bff` → 日志完整输出 `收到终止信号 → 一个服务已优雅退出 → BFF 已关闭`；
  - 缺省密钥变量时 `docker compose config` 直接报错（无弱默认值）。
  - 最终全量测试：**130 passed / 0 failed**（含 4 个 Redis provider 用例，使用 compose Redis）。

### M1-1  P0-1：Redis provider / 多实例（Cache + Lock + Session）✅

- 新增 `src/provider/redis.rs`：
  - `RedisPool`：`redis::aio::ConnectionManager` **惰性建连**（`AppState::new` 保持同步；`AppState::verify_dependencies()` 在启动阶段 PING fail-fast）；
  - `RedisCache`：`GET` / `SET PX` / `DEL`；
  - `RedisLock`：`SET NX PX` + **Lua 校验持有者释放**（防误删他者锁）；
  - `RedisSessionStore`：tower-sessions `SessionStore` 实现（`Record` JSON、key `bff:sess:{id}`、TTL 由 `expiry_date` 推导、ID 冲突重试、损坏数据自清理）。
- `src/provider/session.rs`：`DynSessionStore` 类型擦除包装 + `build_layer(Arc<dyn SessionStore>)`；`AppState.session_store: Arc<dyn SessionStore>`；`validate()` 支持 `memory|redis` 并校验 `redis_url`。
- 测试 `tests/test_redis_providers.rs`（4 例，需 `BFF_TEST_REDIS_URL`）：
  cache 回环+TTL、锁互斥+释放、跨实例会话可见、**双 BFF 实例共享会话（实例 A 登录 Cookie 在实例 B 有效）**。
- 本地验证环境：`docker run -d --name bff-redis -p 127.0.0.1:6379:6379 redis:7-alpine`。

### M1-2  P0-2：回调地址粘性污染修复 ✅（代码+测试；https/E2E 待部署环境）

- `ServerConfig` 新增 `public_base_url` / `trusted_hosts`（含 `validate()` 校验；`prod` 强制二者至少其一）。
- `base_url_from`（`src/oidc/handlers.rs`）重写：**配置了 `public_base_url` 则完全不信任 Host**；回退路径中 Host 必须命中 `trusted_hosts`（未配置时仅允许 loopback），否则 400 拒绝（不再静默拼 `http://<evil>`）。
- `OidcClientManager` 缓存键改为 `(provider_id, base_url)`，`invalidate` 清除该 provider 全部变体；`do_refresh` 改用 `canonical_base_url`（不再硬编码 `127.0.0.1`）——后台刷新无法再污染请求路径的 `redirect_uri`。
- 回归测试 `tests/test_public_base_url.rs`（3 例）：伪造 Host 连续触发 login 仍恒为 `public_base_url` 推导值；未受信任 Host 拒绝/白名单放行；不同 base_url 不共享 client 实例、invalidate 生效。

### M1-3  R4：优雅停机重写 ✅

- `src/main.rs`：**单一信号源**（SIGTERM/SIGINT 各注册一次，watch 广播）替代 3 处独立注册；`JoinSet` 跟踪两个服务，排空窗口 **显式 30s 截止**（替代固定 `sleep(2s)` 硬杀）；注释与行为一致。
- 实测：`kill -TERM` → 日志 `收到终止信号，开始优雅关闭（停止接受新连接）...` → `一个服务已优雅退出` → `BFF 已关闭`，**退出码 0**。

### M1-4  R13：OIDC 出网超时 ✅

- 新增 `src/oidc/http_client.rs`：复用共享 `reqwest::Client` 注入 `request_async` / `discover_async`（官方默认实现**每次新建客户端且无任何超时**）。
- `AppState.oidc_http`：默认 **15s 总超时**（可由 `http_client.timeout` 覆盖），不跟随重定向（防 SSRF）；`client.rs` discovery、callback/refresh 换码、token_exchange discovery 全部切换。
- 故障演练测试 `tests/test_oidc_timeout.rs`：黑洞 IdP（接受 TCP 不响应）下 `/login` 在 <4s 内返回 5xx（实际 ~1s）。

---

## 1.5 第二轮：M1 收尾 + M2 安全主体 + M3 观测 + P0-4（2026-09-27）

### A. 可靠性与容量（M1 收尾）

| 项 | 实施 | 证据 |
| --- | --- | --- |
| **R1** 代理超时 | `http_client.timeout` 默认 **30s**；SSE 改用独立 `http_stream`（无总超时，connect 5s + TCP keepalive 60s）；`route.config.timeout` 路由级覆盖；WS 握手 5s | `state.rs`/`proxy.rs`/`tunnel.rs`；配置 `base.yaml` |
| **R2** 体量上限 | 代理请求体改读 `body_limit.max_bytes`（原硬编码 10MiB）；pipeline/script 1MiB → 同配置；新增 `max_response_bytes`（默认 64MiB）流式累计硬上限 + Content-Length 快速拒绝 | `proxy.rs::read_capped_body`、`route_dispatcher.rs` |
| **R3/R15** 熔断 | 滚动窗口失败计数（成功不清零，间歇故障可触发）；半开**单探针**（含探针超时复位）；`allow(key, threshold)` 路由级阈值生效（键改为路由 path，0=全局默认）；SSE 按**流终态**计（正常结束=成功、读错误=失败） | `circuit_breaker.rs` 单测 5 例；`sse_proxy.rs::stream_with_outcome` |
| **R5** 会话治理 | `session.ttl`（默认 14d）→ Cookie `Max-Age` 与服务端 `expiry_date` 对齐；`sessions` 索引后台 GC（按 store 实际存在性，`session.gc_interval` 默认 10min）；`last_seen` 60s 节流更新 | `provider/session.rs`、`state.rs::gc_sessions_once/run_session_gc/touch_session`、`main.rs` |
| **R14** 内存无界增长 | `InMemoryCache` 改为 moka `Expiry` per-entry TTL（删除旁挂 `entry_ttl` 表，容量约束重新生效）；`InMemoryLock` 引用计数 + Drop 兜底回收（含超时路径） | `provider/cache.rs`（+3 测试）、`provider/lock.rs`（+4 测试，断言锁表归零） |
| **R16/R17/R18** | WS 上游握手超时；登出/管理撤销按 `bff:token_exchange:{sid}:` 前缀清理交换缓存（Redis 后端 SCAN 实现）；管理 API `max_body_bytes`（默认 8MiB） | `tunnel.rs`、`token_exchange.rs::clear_session_cache`、`handlers.rs`、`runtime_api.rs`、`admin/mod.rs` |

### B. 安全加固（M2 主体）

| 项 | 实施 | 证据 |
| --- | --- | --- |
| **S1** 开放重定向 | `validate_redirect` 改为 URL 解析同源校验：拒绝 `\`、`%5C`、控制字符、非 `/` 开头、协议相对/绝对外链 | `handlers.rs` 单测 2 组（含 `/\evil.com`、`\\evil.com`） |
| **S2** 会话固定 | 登录成功后 `session.cycle_id()` 轮换 ID | `handlers.rs::callback` |
| **S3** 管理认证 | token 比较改 SHA-256 摘要常量时间比较；失败按来源 IP 计数（`auth_fail_limit_per_minute`，默认 30/min）超限 429 | `admin/mod.rs` |
| **S4** 管理面加固 | 安全响应头（CSP/XFO/nosniff/Referrer-Policy/HSTS）挂管理路由；Admin UI token 从 localStorage 改 **sessionStorage + 内存** | `admin/mod.rs`、`admin-ui/src/lib/api.ts`、`hooks/useAuth.tsx` |
| **S5** 过度收集 | env 上下文仅在 `from_env` 非空时收集、且只注入显式引用变量（`.` 通配保留但告警）；header 上下文过滤 `cookie/authorization/x-admin-token` | `route_dispatcher.rs` |
| **S6/R7** WS | 仅 `proxy` + `websocket/auto` 路由可升级；按 `auth_required` 强制会话鉴权并向上游握手注入 Bearer；心跳/空闲超时/消息大小上限（1009 关闭）；`connect` 超时 | `business.rs::ws_upgrade_handler`、`tunnel.rs`、`websocket.*` 配置 |
| **S9** 响应头 | 代理与 SSE 统一过滤：hop-by-hop + `access-control-*` + 默认剥离 `set-cookie`（路由级 `forward_set_cookie: true` 可显式放开） | `proxy.rs::should_strip_response_header` |
| **S11** 交换缓存 | Token Exchange 缓存值 AES-256-GCM 加密（fail-closed：加密失败不写缓存；旧明文条目视为 miss） | `token_exchange.rs::read_cache/store_result` |
| **S12** 错误文案 | OIDC discovery/token 交换、代理/SSE 上游错误的对外文案统一为类别描述；细节仅进日志（响应带 `x-request-id` 关联） | `handlers.rs`、`proxy.rs`、`sse_proxy.rs` |
| **S13** IP 解析统一 | 新增 `middleware/client_ip.rs`（nginx `proxy_add_x_forwarded_for` 语义，修正原 `len - trusted - 1` off-by-one；左侧伪造条目被忽略）；认证限流/全局限流（自定义 KeyExtractor）/管理白名单三处复用；管理白名单改**每请求实时读取**（P0-4） | `client_ip.rs`（5 单测）、`rate_limit_skip.rs`、`admin/mod.rs`；`tests/test_ip_rate_limit.rs` 全量重写判据 |

### C. 功能正确性（M2 功能项）

| 项 | 实施 | 证据 |
| --- | --- | --- |
| **F1/F10** 输出映射 | `dispatch` 对 pipeline/script 结果执行 `pick/rename/wrap`；`status_map` 按响应体 `status` 字段查表（`default` 兜底） | `mapping.rs::resolve_status`、`route_dispatcher.rs` |
| **F9** from_path | 新增 `extract_path_param`（段模板 `{name}` 匹配）+ `merge_inputs_full`（优先级 defaults<env<session<header<path<body<query） | `mapping.rs` |
| **F2** 段边界 | `match_route` 前缀匹配改 `p == path \|\| path.starts_with(p + "/")`（`/api` 不再命中 `/api-secret`） | `route_dispatcher.rs` |
| **F5** Provider 管理 | 新增 `DELETE /oidc/providers/:id` 与 `POST /oidc/providers/:id/verify`（真实 discovery，连通失败以 `{ok:false}` 返回）；Admin UI 接入真实调用 | `config_api.rs`、`runtime_api.rs`、`admin-ui/pages/Providers.tsx`；集成测试 `provider_verify_and_delete` |
| **F6** 编排缓存键 | 键加入**调用参数指纹**（排序后 SHA-256 前 16 位），URL 不含用户维度时不再跨用户串数据 | `step.rs` |
| **F11** callback_path | 启动时按 provider `callback_path` 动态注册回调路由（去重、保留 `/auth/callback`）；validate 校验路径合法且不与保留路径冲突 | `business.rs`、`config.rs` |
| **F12** 登出兼容 | 登出端点改用 discovery `end_session_endpoint`（各 IdP 路径不同），失败回退本地清会话并告警；Mock IdP/Spring AS 均提供该元数据 | `oidc/client.rs::end_session_endpoint`、`handlers.rs` |
| **F13** 前端契约 | 演示 SPA 管理 API 路径修正为 `/admin/api/routes`（可配 `window.__BFF_ADMIN_BASE__`）；业务端口 `/admin/api/*` 返回 404 JSON（不再 200+HTML）；管理端未匹配 API 404 JSON | `frontend/src/lib/api.ts`、`business.rs`、`admin/mod.rs` |
| **F8/F14** | 修正 `from_session` 文档（扁平 `sub`）；环境变量优先级已在本轮前修正 | `config.rs` |
| **R9** discovery 缓存 | token_endpoint 缺省时的 discovery 结果缓存 10 分钟 | `token_exchange.rs::resolve_token_endpoint` |

### D. 可观测性与运营（M3）

| 项 | 实施 | 证据 |
| --- | --- | --- |
| **O1** 指标低基数 | `bff_http_requests_total{path}` 标签归一化（路由模板/固定路径/`other`），消除任意 URL 撑爆基数 | `business.rs::metrics_path_label` |
| **O2** 直方图 | 新增 `bff_http_request_duration_seconds`（全局）与 `bff_upstream_request_duration_seconds{upstream,status_class}` | `business.rs`、`proxy.rs` |
| **O3** 链路传播 | W3C `traceparent` 解析/生成/逐跳续接，注入请求头（经代理透传上游）与响应头；非法/缺失则新建根上下文 | `middleware/trace_context.rs`（3 单测） |
| **O4/P0-4** 审计 | 管理写操作统一结构化审计 `admin.audit`（actor=admin-token、来源 IP、method/path/status）；配置变更输出 `admin.config.changed` 含**变更摘要 diff**（routes/pipelines/providers 增删计数、token 是否变更等） | `admin/mod.rs`、`config_api.rs::summarize_change` |
| **R10** /ready | 探测结果缓存 `health.cache_ttl`（默认 1s）；响应裁剪为 `{status, upstreams_total, upstreams_unreachable}`（不再匿名暴露上游 URL/错误串） | `business.rs::readiness` |
| **资产** | Grafana 面板（`deploy/grafana/bff-dashboard.json`）、Prometheus 告警（`deploy/prometheus/bff-alerts.yaml`）、Runbook（`docs/runbook.md`）、部署指南含热生效对照表与 SLO 模板（`docs/production-deployment.md`） | 见文件 |

### E. P0-4 配置持久化与多副本一致性（**P0 关闭项**）

| 能力 | 实施 | 证据 |
| --- | --- | --- |
| 落盘 | `persistence.enabled/path/watch_interval`；管理写操作**先原子写文件（tmp+rename）再应用内存**，失败即拒绝（无内存/磁盘分裂）；文件为脱敏快照（`***` 哨兵） | `state.rs::persist_config/replace_config` |
| 启动恢复 | 加载优先级 `base/分文件 < runtime.yaml < BFF_* 环境变量`；哨兵按环境/基础配置回填 | `config.rs::load` |
| 多副本收敛 | watcher 轮询文件哈希（自身写入跳过；外部变更校验后热重载并 invalidate OIDC 客户端；校验失败仅告警） | `state.rs::run_config_watcher`、`main.rs` |
| 脚本持久化 | `update_script` 在持久化开启时同步写 `config/scripts/<name>`（临时文件+rename） | `config_api.rs` |
| 验证 | 集成测试×3：导入→落盘（无明文密钥）→重启恢复；外部文件变更 ~5s 内热重载；关闭时零写入 | `tests/test_config_persistence.rs` |

### F. 工程化（E）

| 项 | 实施 |
| --- | --- |
| **E4/E14** 供应链 | CI 新增 RustSec `audit` job；`.cargo/audit.toml` 例外清单（serde_yaml 停维=figment 锁定、影响界定与整改计划；idna 0.3=仅测试构建） |
| **E9** 构建依赖 | dev-deps reqwest 改 `default-features=false + rustls-tls` → **整个依赖图移除 openssl-sys**（`cargo tree -e normal` 零命中） |
| **E6** 性能 | 业务/管理路由启用 gzip（谓词排除 `text/event-stream`）；`[profile.release]` lto=thin + codegen-units=1 + strip |
| **E1/E13** CI | （上轮）admin-ui 构建前置 + `-D warnings` 收敛到 clippy 步骤 |
| **E15** 文档 | 补齐 `docs/token-exchange-rfc8693.md`；新增部署/运维/Runbook 文档 |
| **Docker/发布** | release.yml 增加 GHCR 多架构镜像构建推送；本地 HTTPS E2E 栈（`deploy/https/`：nginx TLS 终止 + Mock IdP 示例 + `e2e.sh` 一键验收） |

### G. 本轮实测记录

```text
# 全量测试（含 Redis provider / 跨实例会话 / 持久化 / 安全回归）
env -u HTTP_PROXY -u HTTPS_PROXY BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --all-features
→ 156 passed / 0 failed（另有 2 ignored：依赖 fakesvc）

# 门禁
cargo fmt --all -- --check        → OK
cargo clippy --all-targets --all-features -- -D warnings → OK

# 供应链
cargo tree -e normal -i openssl-sys → 无匹配（纯 rustls）
cargo tree -e normal -i cookie_store → 无匹配（idna 0.3 仅测试链）

# HTTPS + LB 全链路（登录/回调/会话/登出）
bash deploy/https/e2e.sh          → 见 §G.2（部署环境实测记录）
```

#### G.2 HTTPS E2E 实测（本机 Docker，2026-09-27）

> 环境：docker 29.x；nginx 1.27 TLS 终止（:9443）→ bff:8080；宿主机运行 `examples/mock_idp.rs`。

```text
$ bash deploy/https/e2e.sh
== 0) 等待 https://localhost:9443/live 就绪
  ✅ /live = 200（经 nginx TLS 终止）
== 1) redirect_uri 必须基于 public_base_url（防 Host 污染，P0-2）
  ✅ 登录重定向含固定 redirect_uri（https://localhost:9443/auth/callback）
  ✅ 伪造 Host x3 后 redirect_uri 仍为 public_base_url 推导值
== 2) 完整登录（跟随 Mock IdP 自动授权回跳）
  ✅ 登录成功，/api/session = {"logged_in":true}
== 3) 登出（discovery end_session_endpoint + 回跳）
  ✅ 登出成功，/api/session = {"logged_in":false}

🎉 HTTPS 全链路 E2E 通过（登录/回调/会话/登出；经 LB TLS 终止）
```

Mock IdP 侧同源证据（authorize 回跳 + RP-Initiated Logout）：

```text
INFO mock_idp: 自动授权回跳 redirect=https://localhost:9443/auth/callback?code=...&state=...
INFO mock_idp: RP-Initiated Logout 回跳 target=https://localhost:9443/
```

> 生产域名与真实 IdP 的终验仍属上线前 Should 项（见 §2）。
> 构建备注：受限网络环境可用 `CARGO_MIRROR` 构建参数指定 cargo 镜像加速镜像构建。

#### G.3 生产形态 Compose + P0-4 容器闭环实测（2026-09-27）

```text
$ docker compose up -d          # BFF_ENV=prod + Redis + /data/bff 持久化卷
business /live: 200
admin /admin/api/v1/health: 200（X-Admin-Token + IP 白名单）
日志：配置持久化已启用（管理端变更将落盘并支持多副本收敛） path=/data/bff/runtime.yaml

$ curl -X POST ... /admin/api/v1/config/import   # 修改 probe_path 后导入
import: 200 {"status":"applied"}
$ docker exec bff-bff-1 ls -la /data/bff
-rw-r--r-- 1 10001 10001 8700 runtime.yaml     # 非 root（UID 10001）可写

$ docker compose restart bff     # 重启
$ curl ... /config/export | grep -c healthz-check
1                                # 重启后配置不丢（P0-4 闭环）
```

> 实测发现并修复：具名卷初始属主 root → 非 root 容器不可写。
> 修复 = 镜像内预建 `/data/bff` 并 chown 10001（具名卷继承属主）
> + `verify_dependencies` 增加**可写性探针**（不可写即 fail-fast，含处置提示）。
> K8s 侧对应 `fsGroup: 10001`（清单已配）。



## 1.6 第三轮：Keycloak 真实 IdP 契约验证（2026-09-27）

> 对应审计 v2 §1.1「未审计副作用」与 §2「上线前 Should 项」之首：
> **生产 IdP 兼容性未验证（此前仅 Spring Authorization Server / Mock IdP 测过）**。
> 本轮以 Docker 运行 **Keycloak 26**（非 Spring AS 的真实 OIDC 实现）完成契约验证，
> 期间发现并修复 **3 个真实缺陷**（2 个既有实现缺陷 + 1 个 S2 修复引入的回归）。

### A. 新增交付资产（`deploy/keycloak/`）

| 文件 | 作用 |
| --- | --- |
| `realm-bff.json` | Keycloak realm 导入：机密客户端（secret + 强制 PKCE S256）、真实登录用户、`accessTokenLifespan=90s`（刷新断言）、realm 事件记录、已注册的回调/登出回跳 URI |
| `providers.keycloak.yaml` | BFF provider 配置（issuer 与 Keycloak `--hostname` 严格一致；**不跳过验签**——真实 RS256/JWKS） |
| `routes.keycloak.yaml` | 最小路由：`/api/echo` → 回显上游（Bearer 注入断言） |
| `echo-nginx.conf` | 回显 `Authorization` 的最小上游（nginx:1.27-alpine） |
| `docker-compose.keycloak.yml` | 叠加层：Keycloak + 回显上游 + BFF（`BFF_ENV=prod` + Redis + nginx TLS 全量形态） |
| `e2e-keycloak.sh` | 一键验收：起栈→就绪→8 组断言→清理（`KEEP_STACK=1` 保留现场） |
| `README.md` | 使用说明、断言清单与已知本地特性 |

### B. 验收结果（8/8 全绿，2026-09-27 实测）

```text
== 1) Keycloak discovery 契约  ✅ issuer/end_session_endpoint/S256/client_secret_post + BFF 容器内经 host-gateway 真实 discovery
== 2) 登录重定向             ✅ redirect_uri 恒为 public_base_url 推导值（伪造 Host ×2 未污染）
== 3) 真实登录               ✅ Cookie HttpOnly+Secure+SameSite=Lax；授权码+PKCE+RS256/JWKS 验签；管理端会话索引已登记
== 4) Bearer 注入            ✅ 上游收到真实 Keycloak 签发的 access token（iss/azp/typ 校验）
== 5) Redis 会话             ✅ bff 容器重启后登录态不丢（会话不落进程内存）
== 6) 令牌刷新               ✅ SWR 后台刷新（旧≠新 token）+ Keycloak REFRESH_TOKEN 事件
== 7) RP-Initiated Logout    ✅ discovery end_session_endpoint 回跳 + 本地会话清除 + Keycloak LOGOUT 事件
== 8) 汇总                   ✅ 全链路通过
```

### C. 发现并修复的真实缺陷（3 项）

| # | 缺陷 | 根因 | 修复 | 验证 |
| --- | --- | --- | --- | --- |
| K1 | 授权请求 `scope=openid+openid+profile+email` **重复注入**（严格 IdP 可回 `invalid_scope`） | `openidconnect` 的 `authorize_url` 隐式注入 `openid`（crate lib.rs:1065），配置再写一次即重复 | `authorize_scopes()` 大小写不敏感去重（视 `openid` 已存在）+ 空/重复过滤；单测 3 例 | E2E §2 断言 `scope=openid+profile+email` |
| K2 | **管理端会话列表永远为空**（登录后不登记）——S2 会话轮换的回归 | `tower-sessions` 的 `cycle_id()` 会把内部 session id 置 `None`（core 0.12.3 session.rs:848），而 `register_session` 在其后读 `session.id()` → 恒 `None` 静默跳过 | 回调 `cycle_id` 后显式 `session.save()`（store 分配新 id）再登记；`register_session` 对 `None` 增加显式告警；集成回归断言 | E2E §3 列表出现 `"provider":"keycloak"`；`test_oidc_flow::oidc_full_login_flow` |
| K3 | `SameSite=Strict` 使**跨站点 IdP 登录必败**（回调丢会话 Cookie → 401「授权流程不存在」） | 跨站点 IdP 回调是跨站顶层导航，浏览器不携带 Strict Cookie；curl 不强制 SameSite，既有 E2E 无法暴露 | 默认改 `Lax`（`base.yaml` + `SessionConfig::default()`），同站 IdP 可显式回退 Strict；CSRF 主防护为 state+PKCE | E2E 属性断言（HttpOnly+Secure+SameSite=Lax）；Keycloak 登录/回调 Set-Cookie 实测 |

### D. 环境注意（本地验收限定）

- Keycloak 以 `start-dev` + 明文 HTTP + bootstrap/测试凭据运行，**仅限隔离的开发/验收环境**（生产部署应启用 TLS、真实凭据与 `start` 模式）；
- `host.docker.internal` 为「浏览器与 BFF 容器共用同一 issuer 字面量」的本地手段（BFF 经 `extra_hosts`、脚本经 `--resolve`）；真实部署使用统一 DNS 名；
- 沙箱无浏览器，K3 以「Set-Cookie 属性断言 + SameSite 规范」验证；浏览器级跨站实测属部署环境动作。

---

## 2. 剩余事项（上线前 Should / 灰度期迭代）

> P0 阻断项已全部关闭（含 P0-4；P0-2 的“https + LB 全链路”已提供本地 E2E 与 K8s 清单，
> 生产域名 + 真实 IdP 终验属部署环境动作）。

| 优先级 | 项 | 说明与建议 |
| --- | --- | --- |
| ✅ 已完成 | **真实 IdP 兼容性验证** | 2026-09-27 用 Keycloak 26（非 Spring AS）完成契约验证（见 §1.6）：登录/回调/刷新/登出/Bearer 注入/Redis 会话全链路；期间修复 3 个真实缺陷（K1–K3） |
| 上线前 | **外部渗透测试** | 重点：OIDC 回调、`/pipeline`、代理注入、管理面（审计 §M2 DoD） |
| 上线前 | **SLO/负载基线** | 按 `docs/production-deployment.md` §SLO 模板填入目标 QPS/并发并反推限流与 HPA |
| 上线前 | 生产域名 HTTPS 终验 | 用 `deploy/https/` 同构流程在预发执行并留档 |
| P1 迭代 | E5 覆盖率门禁 + 真实验签契约测试；WS/Redis/停机路径补测 | 现有 156 用例（+2 ignored）已覆盖主链路 |
| P1 迭代 | OTel（OTLP）导出 | traceparent 已就绪；引入 exporter 即可衔接上游 span |
| P2 | E14 `serde_yaml` 整改 | 跟踪 figment 上游；或自研合并 + serde_norway |
| P2 | 性能专项 | QuickJS Runtime 复用/池化、ServeDir 缓存、k6 基线数值化 |
| P2 | 灰度（M4） | 1%→10%→50%→100% + 回滚演练 |



## 3. 变更文件清单（历轮增量）

**新增**：`src/middleware/client_ip.rs`、`src/middleware/trace_context.rs`、`tests/test_config_persistence.rs`、`examples/mock_idp.rs`、`deploy/k8s/*`、`deploy/https/*`、`deploy/grafana/bff-dashboard.json`、`deploy/prometheus/bff-alerts.yaml`、`.cargo/audit.toml`、`docs/{production-deployment,runbook,token-exchange-rfc8693}.md`。

**修改**：`src/config.rs`（persistence/websocket/response limit/熔断窗口/admin 加固字段/会话 TTL/路由级超时/回调路径校验/F8 注释）、`src/state.rs`（持久化/GC/watcher/http_stream/哈希）、`src/provider/{cache,lock,session,redis}.rs`、`src/middleware/{circuit_breaker,ip_rate_limit,rate_limit_skip,token_refresh,mod}.rs`、`src/server/{business,proxy,sse_proxy,tunnel,route_dispatcher,mapping,token_exchange}.rs`、`src/oidc/{handlers,client}.rs`、`src/admin/{mod,config_api,runtime_api}.rs`、`src/orchestration/step.rs`、`src/main.rs`、`config/{base.yaml,env/prod.yaml}`、`docker-compose.yml`、`Cargo.toml`（profile/dev-deps）、`.github/workflows/{ci,release}.yml`、`admin-ui/src/{lib/api.ts,hooks/useAuth.tsx,pages/Providers.tsx}`、`frontend/src/lib/api.ts`、`README.md`、本文件。

**第三轮新增**：`deploy/keycloak/{realm-bff.json,providers.keycloak.yaml,routes.keycloak.yaml,echo-nginx.conf,docker-compose.keycloak.yml,e2e-keycloak.sh,README.md}`。

**第三轮修改**：`src/oidc/handlers.rs`（scope 去重 `authorize_scopes`、cycle_id 后显式 save 再登记、登记跳过告警）、`src/config.rs`（会话 Cookie 默认 SameSite Lax）、`config/base.yaml`（同上）、`tests/test_oidc_flow.rs`（会话索引回归断言）、`README.md`、`docs/production-deployment.md`、`CHANGELOG.md`、本文件。

---

## 4. 复现验证（本地）

```bash
# 1) Redis（Docker）
docker run -d --name bff-redis -p 127.0.0.1:6379:6379 redis:7-alpine

# 2) 门禁（156 用例）
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
env -u HTTP_PROXY -u HTTPS_PROXY BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --all-features

# 3) 生产防呆拒绝演示
BFF_ENV=prod ./target/debug/bff            # → Error: admin.auth_token 必须为 ≥32 字符...

# 4) 配置持久化
#    见 tests/test_config_persistence.rs（导入→落盘→重启恢复 / 外部热重载 / 关闭零写入）

# 5) 容器（prod 形态）
export BFF_ADMIN_TOKEN=$(openssl rand -hex 32)
export BFF_SECRET=$(openssl rand -hex 32)
export BFF_SECRET_SALT=$(openssl rand -hex 16)
docker compose up --build

# 6) HTTPS + LB 全链路 E2E（本地）
MOCK_IDP_ISSUER=http://host.docker.internal:9090 cargo run --release --example mock_idp &
bash deploy/https/gen-certs.sh
docker compose -f docker-compose.yml -f deploy/https/docker-compose.https.yml up --build -d
bash deploy/https/e2e.sh

# 7) K8s 清单静态校验（可选）
kubectl apply -k deploy/k8s --dry-run=client

# 8) Keycloak 真实 IdP 契约验证（Docker；首次拉取镜像，约 3–5 分钟）
bash deploy/keycloak/e2e-keycloak.sh            # 结束自动清理；KEEP_STACK=1 保留现场
```

---

## 5. 风险与注意事项

- **有损变更**：Session store 切换（memory→redis）与 `bff_secret` 轮换均等价“全员重登”，
  须按 `docs/production-deployment.md` 迁移预警合并窗口执行（新版已拒绝 `bff_secret` 热更新）。
- **多副本配置收敛依赖共享存储**：`persistence.path` 需挂 RWX 卷；无共享存储时各副本
  仍会“各自持久化、以自身为准”，需回到单副本或引入配置中心。
- `OidcClientManager.get` 写锁内 discovery 仍是潜在放大点（超时与缓存键已修复；未拆锁）。
- 熔断阈值对**已建 key** 固化（路由级阈值在首次调用时生效）；如需在线调整需重启。
- compose 默认 `BFF_ENV=prod` 需宿主注入三个密钥；演示用 `deploy/https/` 为 dev 形态
  （允许 Mock IdP 跳过验签），**不可用于生产**。
- CI `audit` 例外清单（`.cargo/audit.toml`）为已知缺口，新增例外需评审并记录整改计划。
- `deploy/https/e2e.sh` 依赖宿主机 Mock IdP 进程（`examples/mock_idp.rs`），仅用于本地/验收演示。
- `deploy/keycloak/` 为真实 IdP 契约验证资产：Keycloak 以 `start-dev`/明文/测试凭据运行，
  **仅限隔离环境**；生产应使用 `start` 模式 + TLS + 真实凭据与密钥管理。
- **会话 Cookie 默认 `SameSite=Lax`（第三轮 K3）**：跨站点 IdP（不同注册域）回调/登出回跳
  是跨站顶层导航，Strict 会丢 Cookie 导致登录失败；同站 IdP（同一注册域子域）部署可显式改
  `session.same_site: Strict` 收紧（CSRF 主防护为授权流程 state+PKCE，Lax 仍阻断跨站 POST）。
- K2（会话索引登记回归）已修复；若后续升级 `tower-sessions`，需复核 `cycle_id()`/
  `session.id()` 语义（已在 `register_session` 增加 None 告警兜底）。
