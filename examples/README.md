# examples — 示例

- `mock_idp.rs`：最小 OIDC Mock IdP（仅供本地验收演示，如 `deploy/https/` 的 HTTPS + LB 全链路 E2E）。

## 运行

```bash
MOCK_IDP_ISSUER=http://host.docker.internal:9090 cargo run --release --example mock_idp
```

或使用 `make mock-idp`。
