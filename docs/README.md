# 文档索引

| 文档 | 说明 |
| --- | --- |
| [architecture.md](architecture.md) | 架构、模块边界、请求处理链与状态模型 |
| [configuration.md](configuration.md) | 配置参考（文件布局 / 合并顺序 / 各配置段 / 生产防呆） |
| [security-hardening.md](security-hardening.md) | 安全加固清单、验证手段与上线前自评 |
| [deployment.md](deployment.md) | 生产部署：TLS 方案 / K8s / SLO / 热生效对照表 / 上线检查单 |
| [runbook.md](runbook.md) | 告警处置手册 |
| [token-exchange-rfc8693.md](token-exchange-rfc8693.md) | RFC 8693 Token Exchange 设计与运维 |

仓库内的其他验证与交付资产：

- [../deploy/keycloak/README.md](../deploy/keycloak/README.md) — Keycloak 真实 IdP 契约验证（一键 E2E）
- [../deploy/https/](../deploy/https/) — HTTPS + LB 全链路 E2E
- [../benchmark/README.md](../benchmark/README.md) — k6 压测与 SLO 基线
- [../examples/README.md](../examples/README.md) — 示例（Mock IdP）
