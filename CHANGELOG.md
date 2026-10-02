# Changelog

本项目所有重要变更都将记录在此文件中，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Changed

- 文档重构：README 重写为面向外部受众的骨架（新增英文版 README.en.md）；新增 `docs/architecture.md`、`docs/configuration.md`、`docs/security-hardening.md`；原内部审计/进度文档由上述文档取代；`docs/production-deployment.md` 更名为 `docs/deployment.md`
- 构建体验：干净克隆可直接编译——管理端未构建时 `build.rs` 生成占位提示页；新增 `rust-toolchain.toml` 固定 1.93.0
- 元数据：补齐 Cargo / npm 元数据（npm 版本对齐 0.1.0）；新增 `make snapshot`（git archive 快照）与 `make gitleaks`（密钥扫描，例外与理由见 `.gitleaks.toml`）
- 开发/测试组件 Java 包名由 `com.example.*` 迁移至 `cn.byteforce.bff.dev.*`

## [0.1.0] - 2026-09-27

### Added

- 项目初始化：Rust BFF 核心（Axum）+ 管理端 UI（React）+ 测试组件（IAM / fakesvc）
- OIDC 登录（授权码 + PKCE）、令牌刷新（分布式锁防惊群）、登出
- YAML 声明式服务编排（DAG 分层并行、硬超时、fail_fast、HTTP 缓存）
- QuickJS（JavaScript）脚本扩展（沙箱 + `spawn_blocking` 隔离 + 内存/栈/时长上限）
- 反向代理（路由映射、Bearer 注入、熔断）、SSE / WebSocket 透传
- 管理端口（`:8443`）：配置导入/导出（脱敏 + 热重载）、provider / pipeline / 脚本管理、会话列表、Prometheus 指标、内嵌管理 UI
- 可插拔 Provider（缓存 / 锁 / Session，POC 为内存实现）
- 静态 SPA 发布（含前端路由 fallback）
- **Keycloak 真实 IdP 契约验证资产（`deploy/keycloak/`）**：realm 导入、compose 叠加层、
  一键 E2E（授权码+PKCE / RS256+JWKS 验签 / SWR 令牌刷新 / RP-Initiated Logout /
  Bearer 注入 / Redis 会话跨容器重启）
- **SLO/容量基线标定**：压测资产（`benchmark/upstream-nginx.conf`、k6 `capacity` 场景、
  认证路径 COOKIE 与按端点子指标）；实测单实例 ≥10.4k QPS、0 错误、p95 39ms，
  数值与参数反推见 `benchmark/README.md` 与 `docs/deployment.md` §SLO
- **OTel（OTLP/gRPC）追踪导出（O3）**：`telemetry.otlp_endpoint` 配置后启用（默认禁用）；
  span 遵循 OTel HTTP semconv（`http.request`/`http.*`/`otel.kind=server`），入站
  `traceparent` 作为远程父上下文（cross-service 链路在 collector 中可衔接），
  `ParentBased(TraceIdRatioBased)` 采样、TLS 走 rustls、关停时 flush；含进程内 OTLP/gRPC
  collector 端到端契约测试（`tests/test_telemetry.rs`）
- **WebSocket 隧道自动化测试**（补齐覆盖率缺口，`tunnel.rs` 由 0% 覆盖）：
  双向 relay/干净关闭、鉴权与上游 Bearer 注入、路由约束、上游连接失败/握手超时 1011、
  消息大小上限 1009（双向）、心跳保活与空闲超时（`tests/test_ws_tunnel.rs`，9 用例）
- **真实验签契约测试**：进程内 RS256 签名 IdP + JWKS，登录链路在**不跳过验签**下回归；
  覆盖伪造密钥 / `alg:none` 混淆 / nonce 不一致三类攻击拒绝（`tests/test_oidc_signature.rs`）
- **统一路由分发器覆盖补测**：经业务端口真实 HTTP 链路覆盖 Static / Pipeline /
  Script / Proxy 分发与鉴权开关、输入映射（query/body/path/session/defaults 及优先级）、
  输出映射（pick/rename/wrap/status_map 与 default 兜底）、段边界与方法过滤
  （`tests/test_route_dispatch.rs` + 模块内单测 ×6）

### Changed

- 首次开源：补充 LICENSE、CONTRIBUTING、SECURITY、CI 等公开仓库基础设施
- CI 覆盖率门禁按棘轮策略持续上调：`--fail-under-lines 68` → 70 → **75**（lines **79.03%**；`route_dispatcher.rs` 11.87% → 96.47%）
- 出网 HTTP 客户端统一强制 HTTP/1.1（`.http1_only()`）：供应链收敛（旧栈 h2 0.3 生产不可达）
- **OIDC 依赖栈迁移**：`openidconnect` 3.5 → **4.0**（连带 `oauth2` 5.0、`reqwest` 0.12），
  整体移除 h2 0.3 / rustls 0.21 / rustls-webpki 0.101 / hyper 0.14 / idna 0.3 旧栈；
  OIDC 出网改为向 `request_async`/`discover_async` 直传共享 `reqwest::Client`
  （oauth2 5 的 `AsyncHttpClient` 已为该类型实现，删除自研闭包适配层）；
  `CoreClient` typestate 化后，provider discovery 缺 `token_endpoint` 由“换码时失败”提前为
  **构建期快速失败**；对应审计例外（RUSTSEC-2026-0258 / 0098 / 0099 / 0104、RUSTSEC-2024-0421）**清零**

### Fixed

- **CI frontend 作业 pnpm 顺序缺陷**：`setup-node` 的 `cache: pnpm` 要求 pnpm 已在 PATH，
  原顺序（Set up Node 22 → Install pnpm）导致该 job 在
  `Unable to locate executable file: pnpm` 上必挂；现调整为先安装 pnpm 再配置 Node
  （与 rust/coverage 作业同序），CI 首度具备全绿条件
- **映射引擎静默失效（`tests/test_route_dispatch.rs` 补测发现）**：
  `InputMapping.from_path` 从未生效（提取阶段以**目标键**产出，合并层却用模板串
  `path./api/{id}` 当 JSON 路径查询 → 恒 Null，F9 实际未接线）；`from_env` 文档推荐写法
  `env.NAME` 恒为 Null（被拆成 `["env","NAME"]` 嵌套路径查询，仅裸变量名可用）。
  两处均改为专用合并（`apply_path_source` / `apply_env_source`），并以集成 + 单测双层锁定
- **供应链修复**：`h2` → 0.4.19、`rustls` → 0.23.45（修复 RUSTSEC-2026-0258 /
  RUSTSEC-2026-0285）；其余不可修复项（reqwest 0.11 旧栈 TLS / rsa 验签）已在 `.cargo/audit.toml`
  例外界定（含引入链、影响范围与 openidconnect 4 迁移计划）
- **全局限流补液速率（压测发现的可用性缺陷）**：`tower-governor` 0.4.x 的 `per_second(n)` 为
  「每 n 秒补 1 个令牌」周期语义，原实现按「每秒 n 个」直传 → 生产默认 50/s 退化为每 50s 1 个
  （IP 在 burst 耗尽后长时间 429）。现显式换算周期 `1s / per_second`（`src/middleware/rate_limit_skip.rs`），
  429 响应补 RFC 6585 `Retry-After` 头，并补充单测与集成回归测试
  （`test_spa_serving.rs::global_rate_limit_refill_uses_per_second_rate`）
- 授权请求 scope 去重：修复 `openid` 重复注入可能触发严格 IdP `invalid_scope`（Keycloak 契约验证发现）
- 修复会话轮换（`cycle_id`）后的登记回归：管理端会话列表在登录后不再为空
- 会话 Cookie 默认 `SameSite` 由 `Strict` 调整为 `Lax`：跨站点 IdP 回调不再丢会话 Cookie
  （同站 IdP 部署可显式改回 `Strict`）
