# Changelog

本项目所有重要变更都将记录在此文件中，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

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
  数值与参数反推见 `benchmark/README.md` 与 `docs/production-deployment.md` §SLO

### Changed

- 首次开源：补充 LICENSE、CONTRIBUTING、SECURITY、CI 等公开仓库基础设施

### Fixed

- **全局限流补液速率（压测发现的可用性缺陷）**：`tower-governor` 0.4.x 的 `per_second(n)` 为
  「每 n 秒补 1 个令牌」周期语义，原实现按「每秒 n 个」直传 → 生产默认 50/s 退化为每 50s 1 个
  （IP 在 burst 耗尽后长时间 429）。现显式换算周期 `1s / per_second`（`src/middleware/rate_limit_skip.rs`），
  429 响应补 RFC 6585 `Retry-After` 头，并补充单测与集成回归测试
  （`test_spa_serving.rs::global_rate_limit_refill_uses_per_second_rate`）
- 授权请求 scope 去重：修复 `openid` 重复注入可能触发严格 IdP `invalid_scope`（Keycloak 契约验证发现）
- 修复会话轮换（`cycle_id`）后的登记回归：管理端会话列表在登录后不再为空
- 会话 Cookie 默认 `SameSite` 由 `Strict` 调整为 `Lax`：跨站点 IdP 回调不再丢会话 Cookie
  （同站 IdP 部署可显式改回 `Strict`）
