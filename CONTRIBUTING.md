# 贡献指南

感谢你对 BFF 的关注！请先阅读本文，再提交 PR 或 Issue。

交流语言：中文或英文均可（文档以中文为主，另有 [README.en.md](README.en.md)）。

## 项目结构

```text
.
├── Cargo.toml        # Rust BFF 核心（Axum）
├── src/              # 核心源码（oidc / orchestration / provider / server ...）
├── tests/            # Rust 集成测试
├── admin-ui/         # 管理端 UI（React 19 + Vite + shadcn/ui）
├── frontend/         # 演示 SPA（Vite + TypeScript）
├── iam/              # 测试用 OIDC Provider（Spring Boot 3 + Authorization Server）
├── fakesvc/          # 测试用下游服务（Spring Boot 3）
├── config/           # 声明式配置（base.yaml / env / oidc / pipelines / routes）
└── benchmark/        # k6 压测脚本
```

## 环境要求

| 组件   | 版本            | 工具       |
| ------ | --------------- | ---------- |
| Rust   | 1.93.0          | cargo      |
| Java   | 17              | Maven      |
| Node   | 22.x            | pnpm       |

## 开发工作流

```bash
# 1. 克隆并初始化
git clone https://github.com/byteforce-cn/bff.git
cd bff

# 2. 运行 Rust 测试（默认内存 provider，无需外部依赖）
cargo test --all-features

# 3. 构建管理台（编译期内嵌资源；未构建时管理台显示占位提示页，不影响编译与 API）
#    构建后需重新编译 BFF 才会内嵌新产物
cd admin-ui && pnpm install && pnpm build && cd ..

# 4. 启动 bff
cargo run
```

> 演示 SPA（`frontend/`）为运行时资源，构建方式同理；本地联调组件 `iam/` 与 `fakesvc/`
> 的用法见各自目录的 README。
```bash
cd frontend && pnpm install && pnpm build
cd fakesvc && mvn spring-boot:run   # 本地下游服务（:9091）
```

完整命令见 [Makefile](Makefile)。

## 提交 PR 前检查

- [ ] `cargo fmt --all -- --check` 通过
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` 通过
- [ ] `cargo test --all-features` 通过
- [ ] 涉及 Java 模块时 `mvn verify` 通过（`iam/` 与 `fakesvc/`）
- [ ] 涉及前端时 `pnpm build` 通过（`admin-ui/` 与 `frontend/`）
- [ ] 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)（`feat:` / `fix:` / `docs:` / `refactor:` ...）
- [ ] 配置与密钥脱敏：不得包含真实 token / secret / 私钥

## 设计文档

任何涉及架构或行为变更的改动，请在 PR 描述中引用。

## 测试约定

- Rust 集成测试使用内存 provider（`memory`），无需外部依赖，可直接 `cargo test`。
- 涉及 OIDC / 代理的测试可参考 `tests/test_oidc_flow.rs`、`tests/test_full_proxy.rs`。
- 压测脚本见 `benchmark/`，产物输出到 `benchmark/results/`（已 gitignore）。

## 发布（维护者）

- 源码快照 / 分发：`make snapshot`（基于 `git archive`，仅含 tracked 文件，自动排除 `tmp/`、密钥与构建产物）；
- 提交前建议运行 `make gitleaks`（需本机安装 gitleaks）；
- 版本节奏：SemVer + [CHANGELOG.md](CHANGELOG.md)（Keep a Changelog 风格）；推送 `v*` tag 触发 `Release` 工作流（二进制 + GHCR 镜像）。

## AI 辅助开发声明

本项目开发过程中使用了 AI 编码助手；所有 AI 生成内容均经过人工审查与测试验证。
