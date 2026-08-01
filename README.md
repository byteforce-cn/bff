# BFF — 通用型 Backend-For-Frontend 中间件

[![CI](https://github.com/byteforce-cn/bff/actions/workflows/ci.yml/badge.svg)](https://github.com/byteforce-cn/bff/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.93.0-orange)](https://www.rust-lang.org/)
[![Spring Boot](https://img.shields.io/badge/Spring%20Boot-3.4.5-green)](https://spring.io/projects/spring-boot)

基于 **Axum** 的生产级 Backend-For-Frontend 聚合层（POC/alpha）

> ⚠️ **状态**：早期开发版本（POC/alpha）。内置开发密钥仅用于本地，生产环境必须通过环境变量注入真实密钥（见 [SECURITY.md](SECURITY.md)）。

## 🤖 AI 辅助开发

本项目在开发过程中使用以下 AI 工具辅助编码、代码评审与设计讨论：

- **Kimi K3**
- **DeepSeek V4**

当前项目主要用于个人学习、POC和验证模型能力 不可运行于生产环境

> AI 生成内容均经过人工审查与测试验证。

## ✨ 功能特性

- 📄 **静态 SPA 发布**：内嵌前端资源 + 前端路由 fallback
- 🔐 **OIDC 登录**：授权码 + PKCE、令牌刷新（分布式锁防惊群）、登出
- 🔀 **YAML 声明式服务编排**：DAG 分层并行、硬超时、fail_fast、HTTP 缓存
- 📜 **Rhai 脚本扩展**：沙箱 + `spawn_blocking` 隔离 + 操作数/时长上限
- 🔁 **反向代理**：路由映射、Bearer 注入、熔断、限流、SSE / WebSocket 透传
- 🛠️ **管理端口（`:8443`）**：配置导入/导出（脱敏 + 热重载）、provider / pipeline / 脚本管理、会话列表、Prometheus 指标、内嵌管理 UI
- 🧩 **Provider 可插拔**：缓存 / 锁 / Session，POC 全内存零依赖，Redis 为后续扩展点

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
make check           # fmt + clippy + test 全量检查
```

## 📚 文档

| 文档 | 说明 |
| ---- | ---- |
| [benchmark/README.md](benchmark/README.md) | k6 压测说明 |

## 🤝 贡献

欢迎提交 Issue 与 PR！请阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。

## 🔒 安全

发现安全漏洞？请阅读 [SECURITY.md](SECURITY.md)，通过私下渠道报告，勿公开提交。

## 📄 许可证

[MIT](LICENSE) © 2026 [byteforce](https://github.com/byteforce-cn)
