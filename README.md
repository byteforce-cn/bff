# BFF — 通用型 Backend-For-Frontend 中间件

[![CI](https://github.com/byteforce-cn/bff/actions/workflows/ci.yml/badge.svg)](https://github.com/byteforce-cn/bff/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.93.0-orange)](https://www.rust-lang.org/)
[![Spring Boot](https://img.shields.io/badge/Spring%20Boot-3.4.5-green)](https://spring.io/projects/spring-boot)

基于 **Axum** 的 Backend-For-Frontend 聚合层 · **生产化进行中**（原 POC/alpha）

> ⚠️ **状态**：M0 工程止血、M1 状态外置与可用性、M2 安全加固主体、M3 可观测性与运营资产
> 已落地并通过测试（详见 [docs/production-progress.md](docs/production-progress.md)）；
> **P0 阻断项已全部关闭**；**真实 IdP 兼容性已用 Keycloak 26 完成契约验证**
> （登录/回调/刷新/登出/Bearer/Redis 会话全链路，见 [deploy/keycloak/README.md](deploy/keycloak/README.md)）。
> 上线前仍需：外部渗透测试、目标负载/SLO 基线标定
> （见 [docs/production-deployment.md](docs/production-deployment.md) §SLO）。
> 生产环境必须通过环境变量/密钥管理注入真实密钥（见 [SECURITY.md](SECURITY.md)）。
> 部署与运维：[production-deployment.md](docs/production-deployment.md) · [runbook.md](docs/runbook.md)。

## 🤖 AI 辅助开发

本项目在开发过程中使用以下 AI 工具辅助编码、代码评审与设计讨论：

- **Kimi K3**
- **DeepSeek V4**

当前项目已完成主要生产化改造（P0 阻断全部关闭，见上述进度文档）；
真实 IdP 兼容性已用 Keycloak 26 完成契约验证（`deploy/keycloak/`），
渗透测试与 SLO 基线标定仍属上线前 Should 项

> AI 生成内容均经过人工审查与测试验证。

## ✨ 功能特性

- 📄 **静态 SPA 发布**：内嵌前端资源 + 前端路由 fallback
- 🔐 **OIDC 登录**：授权码 + PKCE、令牌刷新（分布式锁防惊群）、登出
- 🔀 **YAML 声明式服务编排**：DAG 分层并行、硬超时、fail_fast、HTTP 缓存
- 📜 **QuickJS 脚本扩展**（JavaScript）：沙箱 + `spawn_blocking` 隔离 + 内存/栈/时长上限
- 🔁 **反向代理**：路由映射、Bearer 注入、熔断（滚动窗口 + 半开单探针）、限流、SSE / WebSocket 透传（WS 鉴权/心跳/上限）
- 🛠️ **管理端口（`:8443`）**：配置导入/导出（脱敏 + 热重载 + **落盘持久化**）、provider / pipeline / 脚本管理、会话列表、Prometheus 指标、内嵌管理 UI
- 🧩 **Provider 可插拔**：缓存 / 锁 / Session，支持 `memory | redis`（Redis 为多实例共享实现，含跨实例会话/锁验证）
- 📈 **可观测性**：请求/上游延迟直方图（低基数标签）、W3C `traceparent` 传播、Grafana 面板与告警规则（`deploy/`）
- 📦 **交付物**：多阶段 Dockerfile、docker-compose（含本地 HTTPS E2E）、K8s 清单（Deployment/Service/Ingress/PDB/HPA/NetworkPolicy/PVC）

## 🏗️ 项目结构

```text
.
├── src/              # Rust BFF 核心（Axum）
│   ├── oidc/         #   OIDC 客户端、令牌处理
│   ├── orchestration/#   DAG 服务编排
│   ├── provider/     #   可插拔缓存 / 锁 / Session
│   ├── server/       #   业务 / 管理 / 代理 / 路由分发
│   ├── middleware/   #   熔断、IP 白名单、令牌刷新
│   └── admin/        #   管理 API
├── tests/            # Rust 集成测试（内存 provider，无外部依赖）
├── admin-ui/         # 管理端 UI（React 19 + Vite + Tailwind 4 + shadcn/ui）
├── frontend/         # 演示 SPA（Vite + TypeScript）
├── iam/              # 测试用 OIDC Provider（Spring Authorization Server，端口 9090）
├── fakesvc/          # 测试用下游服务（Spring Boot 3，端口 9091）
├── config/           # 声明式配置
└── benchmark/        # k6 压测脚本
```

## 🚀 快速开始

### 环境要求

| 组件 | 版本   | 工具 |
| ---- | ------ | ---- |
| Rust | 1.93.0 | cargo |
| Java | 17     | Maven |
| Node | 22.x   | pnpm |

### 运行 BFF

```bash
cargo run            # 业务 :8080  管理 :8443（默认 token: changeme）
```

- 业务端口：`/login` `/auth/callback` `/logout` `/pipeline/:name` `/health` 以及 SPA
- 管理端口：`/admin/api/*`（需 `X-Admin-Token` 头，IP 白名单见 `config/base.yaml`）

### 完整本地链路（可选）

```bash
make build           # admin-ui 构建 + bff release 构建
make iam-run         # 启动测试 OIDC Provider (9090)
cargo run            # 启动 bff
```

`iam/` 与 `fakesvc/` 用于本地联调 OIDC 登录与下游代理，均为测试组件。

## ⚙️ 配置

`config/base.yaml` 为入口 合并其他的配置 §5。环境变量 `BFF_` 前缀可覆盖任意配置（`__` 分层），`BFF_ENV=prod` 时叠加 `config/env/prod.yaml`。

令牌加密密钥通过 `BFF_SECRET` 注入。**POC 内置开发密钥，生产必须覆盖**（详见 [SECURITY.md](SECURITY.md)）。

## 🧪 测试

```bash
cargo test           # 单元 + 全部集成测试（内存 provider，无外部依赖）
# Redis provider / 跨实例会话测试（需本地 Redis，可用 Docker）：
docker run -d --name bff-redis -p 127.0.0.1:6379:6379 redis:7-alpine
BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test test_redis_providers
make check           # fmt + clippy + test 全量检查
```

## 📚 文档

| 文档 | 说明 |
| ---- | ---- |
| [docs/production-deployment.md](docs/production-deployment.md) | 生产部署（TLS 方案/K8s/热生效对照表/SLO） |
| [docs/production-readiness.md](docs/production-readiness.md) | 生产就绪审计报告（v2）与路线图 |
| [docs/production-progress.md](docs/production-progress.md) | 实施进度与验证证据 |
| [docs/runbook.md](docs/runbook.md) | 告警处置手册（Runbook） |
| [docs/token-exchange-rfc8693.md](docs/token-exchange-rfc8693.md) | RFC 8693 Token Exchange 设计与运维 |
| [deploy/keycloak/README.md](deploy/keycloak/README.md) | Keycloak 真实 IdP 契约验证（一键 E2E） |
| [benchmark/README.md](benchmark/README.md) | k6 压测说明 |

## 🤝 贡献

欢迎提交 Issue 与 PR！请阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。

## 🔒 安全

发现安全漏洞？请阅读 [SECURITY.md](SECURITY.md)，通过私下渠道报告，勿公开提交。

## 📄 许可证

[MIT](LICENSE) © 2026 [byteforce](https://github.com/byteforce-cn)
