# 安全策略

## 支持的版本

| 版本   | 支持状态         |
| ------ | ---------------- |
| main   | 安全修复中（beta）|
| v0.x   | 不提供 LTS        |

> 当前项目处于早期开发阶段（POC/alpha），尚未发布正式版本。请勿在生产环境直接使用默认配置。

## 报告漏洞

请**不要**通过公开 Issue 报告安全漏洞。

请通过以下渠道私下报告：

- 邮箱：`security@byteforce.dev`
- GitHub Private Vulnerability Reporting：仓库主页 → **Security** → **Report a vulnerability**

报告中请包含：

1. 影响的组件（BFF 核心 / OIDC / 代理 / Admin API / 脚本引擎）
2. 复现步骤与最小配置
3. 影响评估（如：是否可远程利用、是否可导致凭据泄露）

我们会尽快（目标 72 小时内）确认并给出处理计划。在修复发布前，请勿公开漏洞细节。

## 安全注意事项（给使用者的提醒）

- 所有 `config/` 中的 `changeme` / `bff-secret` / `change-me-in-production` 均为 POC 占位值，**生产环境必须通过环境变量 `BFF_SECRET`、`BFF_SECRET_SALT` 等注入真实密钥**。
- 管理端口（`:8443`）的 `X-Admin-Token` 默认值为 `changeme`，生产环境必须修改并配合 IP 白名单（见 `config/base.yaml`）。
- OIDC `client_secret`、Redis 连接串等敏感配置同样通过环境变量注入，勿硬编码。
- 内置 POC 密钥仅用于本地开发，生产必须覆盖。
