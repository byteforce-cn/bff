# Changelog

本项目所有重要变更都将记录在此文件中，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- 项目初始化：Rust BFF 核心（Axum）+ 管理端 UI（React）+ 测试组件（IAM / fakesvc）
- OIDC 登录（授权码 + PKCE）、令牌刷新（分布式锁防惊群）、登出
- YAML 声明式服务编排（DAG 分层并行、硬超时、fail_fast、HTTP 缓存）
- Rhai 脚本扩展（沙箱 + `spawn_blocking` 隔离 + 操作数/时长上限）
- 反向代理（路由映射、Bearer 注入、熔断）、SSE / WebSocket 透传
- 管理端口（`:8443`）：配置导入/导出（脱敏 + 热重载）、provider / pipeline / 脚本管理、会话列表、Prometheus 指标、内嵌管理 UI
- 可插拔 Provider（缓存 / 锁 / Session，POC 为内存实现）
- 静态 SPA 发布（含前端路由 fallback）

### Changed

- 首次开源：补充 LICENSE、CONTRIBUTING、SECURITY、CI 等公开仓库基础设施

### Fixed

- 无（首个公开版本）
