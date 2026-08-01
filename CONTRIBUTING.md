# 贡献指南

感谢你对 BFF 的关注！请先阅读本文，再提交 PR 或 Issue。

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

# 2. 运行 Rust 测试
cargo test --all-features

# 3. 构建管理端 UI（可选，bff 内嵌 dist）
cd admin-ui && pnpm install && pnpm build && cd ..

# 4. 启动 bff
cargo run
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
