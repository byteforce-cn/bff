# BFF 生产落地进度记录（Production Progress）

> 依据：`docs/production-readiness.md`（审计 v2，基线 `223fd3c`）。本文件记录实际实施与验证证据。
> 编号（P0-x / Sx / Rx / Ex / Fx）与审计报告一一对应。

| 项目 | 内容 |
| ---- | ---- |
| 记录日期 | 2026-09-27 |
| 实施阶段 | **M0 全部** + **M1 核心（P0-1 / P0-2 / R4 / R13）** |
| 门禁状态 | `cargo fmt` ✅ / `cargo clippy -D warnings` ✅ / `cargo test` ✅（**130 passed / 0 failed**） |
| 新增测试 | 15 个用例（P0-5×3、P0-3×4、P0-1×4、P0-2×3、R13×1） |

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

## 2. 未完成 / 下一步（按优先级）

| 优先级 | 项 | 说明 |
| --- | --- | --- |
| **P0** | **P0-4 配置热重载持久化与多实例一致** | 当前仍为进程内存热重载（重启丢失/副本分叉）；管理面审计补全（操作者/差异/查询）；管理面 `ip_whitelist` 冻结问题未修 |
| P0 收尾 | 镜像 + K8s 清单 + HTTPS E2E | Dockerfile/compose 已交付；K8s Deployment/Service/Ingress/PDB/HPA + 探针清单待补；`https 域名 + LB` 全链路验收待部署环境 |
| P1 | S1 开放重定向绕过 / S2 会话轮换 (`cycle_id`) | 认证链路缺陷，建议紧随其后 |
| P1 | S3 admin 常量时间比较 + 独立限流；S4 管理面安全头 + UI token 存储 | 管理面加固 |
| P1 | S6/R7 WS 鉴权/心跳/上限；S9 上游响应头过滤（含 SSE 路径）；S13 限流 IP 解析统一 | 代理与限流语义 |
| P1 | R1/R2 代理超时与三处请求体上限统一（读配置）；R3/R15 熔断语义与半开探针；R5 会话 TTL/Cookie 对齐 + sessions GC；R14 内存 provider 无界增长；R17 登出清交换缓存；R18 管理 API 体上限 | 可靠性与语义 |
| P1 | O1 指标低基数化、O2 延迟直方图、O3 W3C traceparent | 可观测性 |
| P1 | F1/F9/F10 映射引擎（output_mapping/from_path/status_map）、F12 登出端点 discovery、F13 前端 API 路径 | 功能正确性 |
| P2 | E4/E14 供应链（`serde_yaml` 停维）、E5 覆盖率与真实验签、E6 性能（QuickJS Runtime 复用/压缩）、K8s/运维文档 | 工程化 |

> 注：M2/M3/M4 的其余条目（渗透、OTel、灰度）尚未开始。

---

## 3. 变更文件清单（本阶段）

**新增**：`src/provider/redis.rs`、`src/oidc/http_client.rs`、`tests/test_pipeline_auth.rs`、`tests/test_redis_providers.rs`、`tests/test_public_base_url.rs`、`tests/test_oidc_timeout.rs`、`Dockerfile`、`.dockerignore`、`docker-compose.yml`、本文件。

**修改**：`src/state.rs`（provider 装配 / oidc_http / verify_dependencies / bff_secret 热更新拒绝 / replace_config 守卫）、`src/config.rs`（脱敏/回填、provider 校验、prod 防呆、public_base_url、F14 优先级）、`src/oidc/{handlers,client}.rs`、`src/server/business.rs`（P0-5）、`src/admin/{config_api,runtime_api,mod}.rs`、`src/main.rs`（R4 + 依赖自检）、`src/provider/{mod,session}.rs`、`src/orchestration/executor.rs`、`src/middleware/ip_rate_limit.rs`、`src/utils/crypto.rs`、`src/server/{proxy,sse_proxy,tunnel,route_dispatcher}.rs`、`src/server/token_exchange.rs`、`config/{base.yaml,env/prod.yaml}`、`.github/workflows/ci.yml`、`tests/`（common、ip_rate_limit、route_unification、admin_config_import_export、orchestration、script_engine、token_exchange）、`Cargo.toml`（+`redis`、+`time`）。

---

## 4. 复现验证（本地）

```bash
# 1) Redis（Docker）
docker run -d --name bff-redis -p 127.0.0.1:6379:6379 redis:7-alpine

# 2) 门禁
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
env -u HTTP_PROXY -u HTTPS_PROXY cargo test --all-features

# 3) Redis provider 集成测试（含跨实例会话共享）
BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test test_redis_providers

# 4) 生产防呆拒绝演示
BFF_ENV=prod ./target/debug/bff            # → Error: admin.auth_token 必须为 ≥32 字符...

# 5) 容器（compose）
export BFF_ADMIN_TOKEN=$(openssl rand -hex 32)
export BFF_SECRET=$(openssl rand -hex 32)
export BFF_SECRET_SALT=$(openssl rand -hex 16)
docker compose up --build
```

---

## 5. 风险与注意事项

- **Session store 切换（memory→redis）为有损变更**：全部在线会话失效（用户重登），灰度前需公告（审计 §6.5）。
- **`bff_secret` 轮换**同样等价全员重登；当前已拒绝“热切换”，轮换需按“双密钥过渡或合并切换窗口”实施。
- `OidcClientManager.get` 的写锁内做 discovery 仍是全局放大点（R13 已加超时，缓存键已隔离，但未彻底拆锁）。
- compose 默认 `BFF_ENV=prod` 需要宿主环境变量注入三个密钥；`BFF_ADMIN__IP_WHITELIST` 为本地 Docker 网段放宽，生产须按实际入口收紧。
