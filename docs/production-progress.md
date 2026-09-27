# BFF 生产落地进度记录（Production Progress）

> 依据：`docs/production-readiness.md`（审计 v2，基线 `223fd3c`）。本文件记录实际实施与验证证据。
> 编号（P0-x / Sx / Rx / Ex / Fx）与审计报告一一对应。

| 项目 | 内容 |
| ---- | ---- |
| 记录日期 | 2026-09-27（第六轮：OIDC 依赖栈迁移 openidconnect 4.0 + 路由分发器覆盖补测，修复 2 个映射静默失效缺陷） |
| 实施阶段 | **P0 全部关闭**；M2/M3 完成；真实 IdP 兼容性已验证（Keycloak 26）；SLO 基线已标定；OTel OTLP 导出就绪；**审计例外清零（仅余停维类告警）**；上线前仅剩**环境类动作**（外部渗透测试、生产域名 HTTPS 终验） |
| 门禁状态 | `cargo fmt` ✅ / `cargo clippy -D warnings` ✅ / `cargo test` ✅（**206 passed / 0 failed**，另有 2 个依赖 fakesvc 的用例 ignored）/ 覆盖率 **lines 79.03%**（CI 门禁上调至 ≥75，见 §1.9-C）/ `cargo audit --no-fetch` ✅（无漏洞类告警；仅 1 条停维类，含界定） |
| 新增测试 | 第六轮：路由分发 ×13 + 映射回归 ×4 + 分发器单测 ×6（并修复 `from_path`/`from_env` 两个静默失效缺陷） |

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

## 1.7 第四轮：SLO 负载基线标定 + 全局限流语义缺陷修复（2026-09-27）

> 对应审计 §6 路线图「SLO/负载基线」与 P2「性能专项（k6 基线数值化）」。
> 本轮以真实压测完成**单实例容量基线**，并**发现/修复 1 个生产级可用性缺陷（LA1，P1）**。

### A. 压测资产与可复现环境

| 文件 | 作用 |
| --- | --- |
| `benchmark/upstream-nginx.conf` | 最小上游（全路径 200 JSON，隔离上游抖动） |
| `benchmark/k6-load-test.js` | 新增 `capacity` 场景；新增 `COOKIE`（认证路径）与按端点子指标；思考时间参数化；Docker 运行说明 |
| `benchmark/README.md` | 一键复现步骤（上游/Redis/Mock IdP/BFF/Cookie）+ 实测基线数值 |

环境：8 vCPU（i7-1165G7）/ 31GB；BFF release 本地进程（Redis 会话/缓存/锁 + Mock IdP
真实登录）+ nginx 上游 + k6 均为 Docker host 网络；全局限流在压测中放宽（100000/s）以测引擎容量。

### B. 实测基线（全场景 0 错误）

| 场景 | 请求数 | QPS | 错误率 | avg | p95 | p99 |
| --- | ---: | ---: | :--: | --: | --: | --: |
| smoke（1 VU） | 80 | 2.7 | 0.00% | 1.14ms | 2.00ms | 2.36ms |
| baseline（→200 VU） | 311,886 | 2,078 | 0.00% | 0.93ms | 2.89ms | 6.09ms |
| capacity（→800 VU） | 1,572,720 | **10,464** | 0.00% | 9.45ms | 39.41ms | 63.80ms |

分端点 p95（capacity 峰值）：/live 6.6ms、/ready 15.8ms、/api/health 30.5ms、
/api/echo（QuickJS 脚本）32.0ms、/api/users 56.5ms、/api/orders 52.9ms；`dropped_iterations=0`。
结论：**单实例 ≥10.4k QPS**，对 500ms P95 SLO 有 ~9× 余量（k6/Redis 与 BFF 同机，数值偏保守）；
原始数据 `benchmark/results/benchmark-{smoke,baseline,capacity}-*.json`。
SLO 表与限流/HPA 参数反推已填入 `docs/production-deployment.md` §SLO。

### C. 发现并修复的真实缺陷（LA1：全局限流周期语义，P1 可用性）

| # | 缺陷 | 根因 | 修复 | 验证 |
| --- | --- | --- | --- | --- |
| LA1 | 首次 baseline **68.11% 请求 429**（313,320 请求仅 ~10 万通过 ≈ burst 100000，之后全部拒绝） | tower-governor 0.4.3 的 `GovernorConfigBuilder::per_second(n)` 语义是「每 n 秒补 1 个令牌」（周期），实现按「每秒 n 个」直传 → 生产默认 50/s 实为**每 50 秒 1 个**（收紧 2500×） | `src/middleware/rate_limit_skip.rs` 新增 `governor_period()` 显式换算 `1s/N`（含上下界钳制），不再直接透传；自定义错误处理为 429 补 **`Retry-After`**（RFC 6585，与认证限流一致）；`config/base.yaml` 增加语义警示注释 | 单测 ×2（周期换算/零周期边界）；集成回归 `test_spa_serving.rs::global_rate_limit_refill_uses_per_second_rate`（旧语义下确定失败 429 → 修复后 700ms 复放行）+ 429 `Retry-After` 头断言；修复后同场景 0 错误 |

> 长期未被发现的原因：既有测试仅用 `per_second: 1`（周期 1s 与「每秒 1 个」巧合等价），
> 无测试覆盖 N>1 的补液速率；认证端点限流为自研令牌桶（语义正确），不受影响。
> 影响面：若不修复，生产默认下任一 IP 在 burst（500）耗尽后将被限至 1 req/50s；
> 升级 tower-governor 时需注意返回值语义（勿回退为直接透传）。

### D. E5 覆盖率门禁（cargo-llvm-cov）

- 实测（含 Redis 用例全量）：**lines 70.71% / regions 68.64% / functions 67.29%**（9,472 regions）。
- CI 新增 `coverage` job：`cargo llvm-cov --all-features --summary-only --fail-under-lines 68`（含 Redis service；
  棘轮策略——新增测试后逐步上调）；Makefile 新增 `make coverage`。
- 已知低覆盖热点（后续补测优先级）：`server/tunnel.rs` **0%**（WS 隧道无自动化用例，对应 §2「WS 补测」）、
  `server/route_dispatcher.rs` 17%（路由分发大量分支仅走部分路径）、`provider/redis.rs` 64%、`server/business.rs` 58%。

### E. 本轮变更文件

- 修改：`src/middleware/rate_limit_skip.rs`、`tests/test_spa_serving.rs`、`config/base.yaml`、
  `benchmark/{README.md,k6-load-test.js}`、`docs/production-deployment.md`、`docs/runbook.md`、
  `.github/workflows/ci.yml`、`Makefile`、`README.md`、`CHANGELOG.md`、本文件；
- 新增：`benchmark/upstream-nginx.conf`；
- 压测产物（不入库）：`benchmark/results/*.json`。

---

## 1.8 第五轮：P1 遗留收口——WS 补测 / 真实验签 / OTel 导出 / 供应链修复（2026-09-27）

> 对应审计 v2 与 §2 剩余事项中的 P1 迭代项：E5 覆盖率热点（`tunnel.rs` 0%）、
> 「真实验签契约测试」、「OTel（OTLP）导出」；并针对第五轮引入/发现的供应链告警做修复与界定。

### A. WebSocket 隧道自动化测试（E5 热点：`tunnel.rs` 0% → **84.01%**）

新增 `tests/test_ws_tunnel.rs`（9 用例，进程内 WS 回显上游 + 黑洞上游，全部无需外部依赖）：

| # | 用例 | 断言要点 |
| --- | --- | --- |
| 1 | `ws_echo_relay_and_clean_close` | 文本/二进制双向 relay；客户端 Close 透传上游（关闭帧记录）；匿名路由不注入 Authorization |
| 2 | `ws_auth_required_rejects_anonymous_and_injects_bearer` | S6：无会话升级 → 401 且不触及上游；有会话 → 上游握手收到 `Bearer test-access-token` |
| 3 | `ws_route_constraints` | 非 `websocket|auto` 模式 → 400；无匹配路由 → 404 |
| 4 | `ws_upstream_connect_failure_closes_1011` | 上游拒连 → 客户端收到 1011 Close |
| 5 | `ws_upstream_handshake_timeout_closes_1011` | R16：黑洞上游 + `connect_timeout=500ms` → 1011，且控制在 ~1s 内返回 |
| 6 | `ws_oversized_client_message_closed_with_1009` | 客户端超限消息 → 上游收到 1009 |
| 7 | `ws_oversized_upstream_message_closes_client_1009` | 上游超限消息 → 客户端收到 1009 |
| 8 | `ws_heartbeat_keeps_tunnel_alive_past_idle_timeout` | 心跳保活：空闲窗口（2×idle）内无业务消息隧道不关，之后仍可收发 |
| 9 | `ws_idle_timeout_closes_tunnel_when_no_heartbeat` | 禁用心跳时空闲超时必触发关闭 |

### B. 真实验签契约测试（P1「真实验签契约测试」）

既有 mock IdP 以 `alg:none` + `insecure_skip_id_token_verification=true` 运行，Keycloak E2E 又依赖 Docker；
新增 `tests/test_oidc_signature.rs`（5 用例，**cargo test 门禁内**回归完整验签）：

| # | 用例 | 断言要点 |
| --- | --- | --- |
| 1 | `rs256_jwks_valid_login_succeeds` | 进程内 RS256 签名 IdP + JWKS，**不跳过验签** → 登录成功、会话登记 |
| 2 | `rs256_wrong_key_rejected` | 签名密钥与 JWKS 不匹配（伪造/轮换攻击）→ 401、不建会话 |
| 3 | `rs256_alg_none_rejected` | `alg:none` 无签名令牌（JWT 混淆攻击）→ 401、不建会话 |
| 4 | `rs256_wrong_nonce_rejected` | 签名合法但 nonce 不一致（重放/串会话）→ 401、不建会话 |
| 5 | `idp_jwks_contract_is_served_and_parseable` | JWKS 结构（kty/alg/kid/n/e）与 discovery 仅公布 RS256 |

实现说明：内嵌固定 RSA 测试密钥（`openssl genpkey` 生成、仅测试用途，非秘密），避免 debug 构建下
RSA 生成耗时（~10s → 0.4s）；`rsa` 作为 dev-dependency（与 openidconnect 传递依赖同版本，无新增供应链面）。

### C. OTel（OTLP/gRPC）追踪导出（M3 旗舰遗留项）

| 项 | 实施 | 证据 |
| --- | --- | --- |
| 配置与出口 | `telemetry.{otlp_endpoint,service_name,sample_ratio}`；OTLP/gRPC（tonic）；TLS 走 **rustls**（tls-roots 读系统证书库，不引入 openssl）；endpoint 为空完全禁用（默认） | `src/telemetry.rs`、`config/{base,env/prod}.yaml`、`src/config.rs::validate` |
| span 语义 | `BffMakeSpan`：`http.request` + `http.method/target/status_code` + `otel.kind=server`；`RecordStatusOnResponse` 于响应阶段补 `http.status_code`（保留默认完成日志） | `src/middleware/trace_context.rs`、`src/server/business.rs` |
| 跨服务衔接 | 入站 `traceparent` → OTel 远程父上下文（`context_from_traceparent`，含严格长度/字符校验）；出站/响应 `traceparent` 优先取本跳 span 的 OTel 上下文（`current_span_traceparent`）→ **collector 中的 span 树与上游收到的 parent 严格一致**；未启用导出时自动回退既有手动上下文（行为不变） | 同上；`tests/test_telemetry.rs` |
| 采样 | `ParentBased(TraceIdRatioBased(sample_ratio))`：有上游上下文跟随上游采样位（W3C 语义），根请求按比例采样 | 同上 |
| 关停 | `TelemetryHandle::shutdown_async`（spawn_blocking）：处置 SDK `BatchSpanProcessor::shutdown()` 的 `futures_executor::block_on` 在 **current_thread 运行时死锁** 的坑（含 provider drop 路径）；main 在退出前 flush | `src/telemetry.rs`、`src/main.rs` |
| 层序陷阱 | trace 中间件必须位于 TraceLayer 之内、**任何创建子 span 的层之外**（实测 tower-sessions 会创建 `call` span，导致 traceparent 的 span-id 与导出 span 不一致）；已用端到端用例锁定 | `src/server/business.rs` 注释 + `http_request_span_exported_and_linked` |

新增 `tests/test_telemetry.rs`（7 用例，进程内 **tonic OTLP collector** 真收导出）：

1. 默认禁用（`None`，无出站）；2. 非法 endpoint fail-fast；3. 配置校验（scheme/采样率边界）；
4. OTLP 端到端导出：`service.name`、span 名/属性、**trace_id 延续 + parent_span_id == 入站**；
5. 传播语义：同 trace、新 span_id、采样位如实（01/00）；6. `BffMakeSpan` 头→父链衔接；
7. **生产接线端到端**：真实 HTTP 请求的响应 `traceparent` span-id == collector 中导出 span 的 span_id，
   `http.status_code=200` 等属性齐全。

### D. 供应链（E4）：审计告警修复与例外界定

第五轮加入 OTel（tonic 栈）后 `cargo audit` 暴露 7 项；按「能修必修、不能修必须界定」处理：

| 项 | 处置 | 结果 |
| --- | --- | --- |
| h2 0.4.15（RUSTSEC-2026-0258） | `cargo update` → **0.4.19** | 已修复 |
| rustls 0.23.43（RUSTSEC-2026-0285） | `cargo update` → **0.23.45** | 已修复 |
| h2 0.3.27（旧栈，0.3 线 EOL 无修复） | 共享出网客户端统一 **`.http1_only()`**（`state.rs::build_http_client`）→ h2 0.3 在生产**不可达**；审计例外（含界定） | 收敛 + 例外 |
| rustls-webpki 0.101.7 ×3（名称约束/CRL，来自 reqwest 0.11 → rustls 0.21） | 审计例外：仅出网证书校验、0.101 栈不启用 CRL、名称约束绕过需受信 CA 链（内网 IdP/上游场景）；P1 迁移计划见 §2 | 例外 |
| rsa 0.9.10 Marvin（RUSTSEC-2023-0071，上游无修复） | 审计例外：Marvin 针对 RSA 私钥操作；生产仅公钥验签（openidconnect），不执行私钥运算；测试签发用内嵌测试密钥 | 例外 |

供应链约束保持：`cargo tree -e normal -i openssl-sys` / `native-tls` 零命中（E9）；例外清单
（`.cargo/audit.toml`）均含「引入链 + 影响界定 + 整改计划」，CI `audit` job 读取后通过。

### E. 覆盖率门禁与实测记录

```text
# 门禁（第五轮全绿）
cargo fmt --all -- --check                                   → OK
cargo clippy --all-targets --all-features -- -D warnings     → OK
BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --all-features
  → 183 passed / 0 failed（另有 2 ignored：依赖 fakesvc）
cargo audit                                                  → 通过（2 条 allowed 级 unmaintained 告警）
cargo tree -e normal -i openssl-sys / native-tls             → 无匹配（rustls 全栈）
cargo llvm-cov --all-features --summary-only                 → lines 72.82% / regions 70.75% / functions 75.12%
  热点：server/tunnel.rs 0% → 84.01%；telemetry.rs 94.37%；server/proxy.rs 81.46%
  （仍低：server/route_dispatcher.rs 11.87%，P1 后续补测）
CI 覆盖率门禁：--fail-under-lines 68 → **70**（棘轮策略）
```

### F. 本轮变更文件

- 新增：`src/telemetry.rs`、`tests/test_ws_tunnel.rs`、`tests/test_oidc_signature.rs`、`tests/test_telemetry.rs`；
- 修改：`src/{main.rs,lib.rs,state.rs,config.rs}`、`src/middleware/trace_context.rs`、`src/server/business.rs`、
  `config/env/prod.yaml`、`Cargo.toml`/`Cargo.lock`、`.cargo/audit.toml`、`.github/workflows/ci.yml`、
  `CHANGELOG.md`、`README.md`、`docs/production-deployment.md`、本文件。

---

## 1.9 第六轮：OIDC 依赖栈迁移（openidconnect 4.0）+ 分发器覆盖补测（2026-09-27）

> 对应审计 §2「P1 迭代」最后两项：openidconnect 3.5 → 4.0 迁移（清零 4 条审计例外）
> 与 `route_dispatcher.rs` 覆盖率补测（11.87%）。本轮由补测**发现并修复 2 个映射静默失效缺陷**。

### A. OIDC 依赖栈迁移（openidconnect 4.0 / oauth2 5 / reqwest 0.12）

| 项 | 变更 | 说明 |
| --- | --- | --- |
| 依赖升级 | `openidconnect 3.5 → 4.0.1`、`oauth2 4.4 → 5.0.0`、`reqwest 0.11 → 0.12.28`（含 dev-deps） | 整体移除 h2 0.3 / rustls 0.21 / hyper 0.14 / rustls-webpki 0.101 / idna 0.3 旧栈（`cargo tree` 零命中） |
| 出网客户端 | 删除 `src/oidc/http_client.rs` 闭包适配层 | oauth2 5 起 `reqwest::Client` 直接实现 `AsyncHttpClient`：`request_async`/`discover_async` 直传 `&state.oidc_http`（R13 语义不变：15s 总超时、禁重定向、连接池复用） |
| typestate | `CoreClient` 4.0 起为 typestate 泛型：新增 `BffCoreClient` 别名；`build_client` 以 discovery 元数据的 `token_endpoint` 经 `set_token_uri` 升级 | provider 缺 `token_endpoint` 由「换码时失败」提前为**构建期 fail-fast** |
| 审计例外 | 删除 RUSTSEC-2026-0258（h2 0.3）、RUSTSEC-2026-0098/0099/0104（rustls-webpki 0.101）、RUSTSEC-2024-0421（idna 0.3） | `cargo audit` 现无漏洞类告警；新增 1 条**停维类**（rustls-pemfile ← tonic，已界定） |

验证：`cargo tree` 旧栈零命中；`cargo audit --no-fetch` 通过；全量测试全绿；
**Keycloak 26 真实 IdP E2E 8/8 全绿**（见 §1.9-D）。

### B. 路由分发器覆盖补测（P1 热点）+ 2 个真实缺陷修复

**新增测试**：`tests/test_route_dispatch.rs`（13 例，业务端口真实 HTTP 链路：Static/Pipeline/Script 分发、
鉴权开关、输入映射 query/body/path/session/env 与优先级、输出映射 pick/rename/wrap/status_map、
段边界与方法过滤）；`route_dispatcher.rs` 模块内单测 ×6（match_route 段边界/最长前缀/方法大小写/
尾斜杠归一、inputs_to_string_map、build_env_context）。

**由补测发现并修复**：

| # | 缺陷 | 根因 | 修复 | 验证 |
| --- | --- | --- | --- | --- |
| M1 | `InputMapping.from_path` **从未生效**（F9 接线错误，静默丢参） | 提取阶段 `path_json` 以**目标键**产出，合并层 `apply_source` 却用模板串（`path./api/{id}`）当 JSON 路径查询 → 恒 Null | `mapping.rs` 新增 `apply_path_source`（按目标键取值） | 集成 `script_route_extracts_inputs_across_sources` + 单测 ×2 |
| M2 | `from_env` 文档推荐写法 `env.NAME` **恒为 Null**（仅裸变量名可用） | 上下文以变量名为键，合并层把 `env.NAME` 拆成 `["env","NAME"]` 查嵌套路径 | `mapping.rs` 新增 `apply_env_source`（`env.NAME`/裸名/`"."` 通配均正确解析） | 集成 ×1（含 S5 未引用变量不注入断言）+ 单测 ×2 |

> 两个缺陷均为「配置可写、UI 可编、运行期静默变空值」类型：M1 使 F9（第二轮声明完成）实际未生效，
> M2 使 S5 建议的脱敏写法不可用——均为**静默失效**，排障成本高；现均以集成 + 单测双层锁定。

### C. 覆盖率与门禁（E5 棘轮）

```text
BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo llvm-cov --all-features --summary-only
→ lines 79.03% / regions 77.72% / functions 73.04%
热点清零：server/route_dispatcher.rs 11.87% → 96.47%；server/mapping.rs → 96.55%
CI 覆盖率门禁：--fail-under-lines 70 → 75（棘轮策略）
```

### D. Keycloak 真实 IdP E2E（迁移后契约回归）

```text
$ FORCE_BUILD=1 bash deploy/keycloak/e2e-keycloak.sh   # 镜像构建（受限网络经 CARGO_MIRROR，见 §4）
== 1) discovery 契约          ✅ issuer / end_session_endpoint / S256 / client_secret_post + BFF 容器内真实 discovery
== 2) 登录重定向              ✅ redirect_uri 恒为 public_base_url 推导值（伪造 Host ×2 未污染）
== 3) 真实登录                ✅ 授权码+PKCE+RS256/JWKS 验签；Cookie HttpOnly+Secure+SameSite=Lax；会话登记
== 4) Bearer 注入             ✅ 上游收到真实 Keycloak access token（iss/azp/typ 校验）
== 5) Redis 会话              ✅ bff 容器重启后登录态不丢
== 6) 令牌刷新                ✅ SWR 轮换（旧≠新）+ Keycloak REFRESH_TOKEN 事件
== 7) RP-Initiated Logout     ✅ 本地会话清除 + Keycloak LOGOUT 事件
== 8) 汇总                    ✅ 全链路通过
```

> 迁移后真实 IdP 契约无回退；Q1（scope 去重）/K2（会话登记）/K3（SameSite）等既有修复继续有效。

### E. 本轮变更文件

- 新增：`tests/test_route_dispatch.rs`；
- 删除：`src/oidc/http_client.rs`（被 oauth2 5 直传客户端取代）；
- 修改：`Cargo.toml`/`Cargo.lock`（openidconnect 4.0/oauth2 5/reqwest 0.12）、`src/oidc/{client,handlers,mod}.rs`、
  `src/server/{token_exchange,mapping,route_dispatcher}.rs`、`src/admin/runtime_api.rs`、`src/state.rs`、
  `tests/{test_mapping_engine,test_oidc_timeout}.rs`、`.cargo/audit.toml`、`.github/workflows/ci.yml`（覆盖率门禁 → 75）、
  `CHANGELOG.md`、`README.md`、`docs/production-deployment.md`、本文件。

---

## 2. 剩余事项（上线前 Should / 灰度期迭代）

> P0 阻断项已全部关闭（含 P0-4；P0-2 的“https + LB 全链路”已提供本地 E2E 与 K8s 清单；
> 真实 IdP 已用 Keycloak 26 完成契约验证；**生产域名 HTTPS 终验**属部署环境动作）。
> 第六轮完成后，工程仓库内可推进的 P1 项**已全部收口**（WS 补测 / 真实验签 / OTel / 供应链 /
> openidconnect 4.0 迁移 / 分发器补测）；剩余为**环境类动作**与 P2 灰度期迭代。

| 优先级 | 项 | 说明与建议 |
| --- | --- | --- |
| ✅ 已完成 | **真实 IdP 兼容性验证** | 2026-09-27 用 Keycloak 26（非 Spring AS）完成契约验证（见 §1.6）：登录/回调/刷新/登出/Bearer 注入/Redis 会话全链路；期间修复 3 个真实缺陷（K1–K3） |
| ✅ 已完成 | **SLO/负载基线（本地标定）** | 2026-09-27 实测单实例 ≥10.4k QPS、0 错误、p95 39ms（见 §1.7）；生产/预发环境复测仍建议（同名压测脚本可直接复用：`benchmark/README.md`） |
| 上线前 | **外部渗透测试** | 重点：OIDC 回调、`/pipeline`、代理注入、管理面（审计 §M2 DoD）。第五轮已入库内测回归：伪造密钥/`alg:none`/nonce 攻击拒绝、开放重定向单测、IP 伪造限流用例 |
| 上线前 | 生产域名 HTTPS 终验 | 用 `deploy/https/` 同构流程在预发执行并留档 |
| ✅ 已完成 | E5 覆盖率（第六轮再上调） | lines 第五轮 72.82% → 第六轮 **79.03%**（regions 77.72%）；CI 门禁 68 → 70 → **75**；`route_dispatcher.rs` 11.87% → **96.47%**、`mapping.rs` → 96.55%；仍低：`admin/config_api.rs` 34%（P2 可再补） |
| ✅ 已完成 | 真实验签契约测试（第五轮） | 进程内 RS256/JWKS，不跳过验签 + 三类攻击拒绝（`tests/test_oidc_signature.rs`） |
| ✅ 已完成 | OTel（OTLP）导出（第五轮） | OTLP/gRPC + traceparent 衔接（span 树一致）+ ParentBased 采样 + 关停 flush + 端到端契约测试（`tests/test_telemetry.rs`） |
| ✅ 已完成 | **openidconnect 3.5 → 4.0 迁移**（第六轮，连带 oauth2 5、reqwest 0.12） | 整体移除旧栈（h2 0.3 / rustls 0.21 / rustls-webpki 0.101 / hyper 0.14 / idna 0.3）；审计例外清零（仅余 1 条停维类）；Keycloak E2E 契约回归 8/8（见 §1.9） |
| ✅ 已完成 | `route_dispatcher.rs` 覆盖率补测（第六轮） | 11.87% → **96.47%**（13 集成 + 6 单测）；**连带发现并修复 2 个映射静默失效缺陷（M1 from_path / M2 from_env）**；`business.rs` 69.82% → 71.28%（剩余分支属灰度期 P2） |
| P2 | E14 `serde_yaml` 整改 | 跟踪 figment 上游；或自研合并 + serde_norway |
| P2 | 性能专项 | k6 基线已数值化（§1.7）；QuickJS 池化经实测**无需**（脚本路径 p95 32ms @10k QPS，SLO 余量 ~15×）；ServeDir 缓存按需评估 |
| P2 | 灰度（M4） | 1%→10%→50%→100% + 回滚演练 |



## 3. 变更文件清单（历轮增量）

**新增**：`src/middleware/client_ip.rs`、`src/middleware/trace_context.rs`、`tests/test_config_persistence.rs`、`examples/mock_idp.rs`、`deploy/k8s/*`、`deploy/https/*`、`deploy/grafana/bff-dashboard.json`、`deploy/prometheus/bff-alerts.yaml`、`.cargo/audit.toml`、`docs/{production-deployment,runbook,token-exchange-rfc8693}.md`。

**修改**：`src/config.rs`（persistence/websocket/response limit/熔断窗口/admin 加固字段/会话 TTL/路由级超时/回调路径校验/F8 注释）、`src/state.rs`（持久化/GC/watcher/http_stream/哈希）、`src/provider/{cache,lock,session,redis}.rs`、`src/middleware/{circuit_breaker,ip_rate_limit,rate_limit_skip,token_refresh,mod}.rs`、`src/server/{business,proxy,sse_proxy,tunnel,route_dispatcher,mapping,token_exchange}.rs`、`src/oidc/{handlers,client}.rs`、`src/admin/{mod,config_api,runtime_api}.rs`、`src/orchestration/step.rs`、`src/main.rs`、`config/{base.yaml,env/prod.yaml}`、`docker-compose.yml`、`Cargo.toml`（profile/dev-deps）、`.github/workflows/{ci,release}.yml`、`admin-ui/src/{lib/api.ts,hooks/useAuth.tsx,pages/Providers.tsx}`、`frontend/src/lib/api.ts`、`README.md`、本文件。

**第三轮新增**：`deploy/keycloak/{realm-bff.json,providers.keycloak.yaml,routes.keycloak.yaml,echo-nginx.conf,docker-compose.keycloak.yml,e2e-keycloak.sh,README.md}`。

**第三轮修改**：`src/oidc/handlers.rs`（scope 去重 `authorize_scopes`、cycle_id 后显式 save 再登记、登记跳过告警）、`src/config.rs`（会话 Cookie 默认 SameSite Lax）、`config/base.yaml`（同上）、`tests/test_oidc_flow.rs`（会话索引回归断言）、`README.md`、`docs/production-deployment.md`、`CHANGELOG.md`、本文件。

**第四/五轮新增**：`benchmark/upstream-nginx.conf`、`src/telemetry.rs`、`tests/{test_ws_tunnel,test_oidc_signature,test_telemetry}.rs`。

**第四/五轮修改**：`src/middleware/{rate_limit_skip,trace_context}.rs`、`src/server/business.rs`（OTel span 与层序）、`src/state.rs`（出网 `.http1_only()`）、`src/main.rs`（OTel 接线/关停 flush）、`src/config.rs`（telemetry）、`src/lib.rs`、`config/env/prod.yaml`、`.cargo/audit.toml`、`.github/workflows/ci.yml`（覆盖率门禁 → 70）、`Cargo.toml`/`Cargo.lock`、`benchmark/{README.md,k6-load-test.js}`、`docs/production-deployment.md`、`README.md`、`CHANGELOG.md`、本文件；逐项证据见 §1.7-E / §1.8-F。

**第六轮新增**：`tests/test_route_dispatch.rs`。
**第六轮删除**：`src/oidc/http_client.rs`（oauth2 5 直传 `reqwest::Client`，闭包适配层不再需要）。
**第六轮修改**：`Cargo.toml`/`Cargo.lock`（openidconnect 4.0.1 / oauth2 5.0.0 / reqwest 0.12.28）、`src/oidc/{client,handlers,mod}.rs`、`src/server/{mapping,route_dispatcher,token_exchange}.rs`、`src/admin/runtime_api.rs`、`src/state.rs`（`.http1_only()` 注释）、`tests/{test_mapping_engine,test_oidc_timeout}.rs`、`.cargo/audit.toml`、`.github/workflows/ci.yml`（覆盖率门禁 → 75）、`CHANGELOG.md`、`README.md`、`docs/production-deployment.md`、本文件；逐项证据见 §1.9。

---

## 4. 复现验证（本地）

```bash
# 1) Redis（Docker）
docker run -d --name bff-redis -p 127.0.0.1:6379:6379 redis:7-alpine

# 2) 门禁（206 用例；CI 另含 audit + 覆盖率门禁 ≥75）
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
env -u HTTP_PROXY -u HTTPS_PROXY BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --all-features
cargo audit --no-fetch              # 读取 .cargo/audit.toml（例外均含界定）
env -u HTTP_PROXY -u HTTPS_PROXY BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo llvm-cov --all-features --summary-only

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
# 受限网络：镜像内 cargo 走镜像源（Cargo.lock 变更后依赖层需全量重编，
# 否则 crates.io 下载会显著拖慢构建）：
#   export CARGO_MIRROR='sparse+https://rsproxy.cn/index/'
#   FORCE_BUILD=1 bash deploy/keycloak/e2e-keycloak.sh
# 重跑前建议清理残留（占 8180/6379/9443 等端口；需注入三个 BFF_* 密钥变量）：
#   docker compose -f docker-compose.yml -f deploy/keycloak/docker-compose.keycloak.yml down --remove-orphans
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
