# iam — 本地 OIDC Provider（开发/测试组件）

基于 Spring Authorization Server 的 **测试用** OIDC Provider（端口 `:9090`），
用于本地联调登录链路与自动化契约验证。

> ⚠️ 这是开发/测试夹具，**不是生产组件**，请勿将其作为生产 IdP 部署。
> BFF 的生产 IdP 兼容性以 Keycloak 真实契约验证为准（见 `../deploy/keycloak/`）。

## 运行

```bash
mvn spring-boot:run      # 或 make iam-run
```

- issuer 与本地 BFF 的 provider 配置对齐（`config/oidc/providers.yaml`）；
- Java 包名 `cn.byteforce.bff.dev.iam`；Maven 坐标 `cn.byteforce.bff.dev:iam`。
