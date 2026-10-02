# 演示 SPA（demo SPA）

用于本地联调与契约验证的极简 SPA（Vite + TypeScript，零框架依赖）：
演示 BFF 的登录、反向代理、SSE、WebSocket 等能力。

- 构建产物 `frontend/dist` 是 BFF 的**运行时**静态资源（`config/base.yaml` 的 `spa.dir`），由业务端口 `:8080` 托管；
- 未构建时 SPA 路径返回 404，不影响 API。

## 开发

```bash
pnpm install
pnpm dev      # Vite dev server（含对 BFF 的代理配置，见 vite.config.ts）
pnpm build
```
