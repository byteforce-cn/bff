# Admin UI（管理台）

BFF 管理台前端：React 19 + Vite + Tailwind 4 + shadcn/ui。

- 编译期由 RustEmbed 内嵌进 BFF 二进制（`src/admin/mod.rs`）——**构建后需重新编译 BFF 才会生效**；
- 未构建时管理台显示占位提示页（`build.rs` 兜底），不影响编译与 API；
- 调用的管理 API 挂在独立管理端口 `:8443` 的 `/admin/api/*`，需要 `X-Admin-Token`。

## 开发

```bash
pnpm install
pnpm dev      # 本地开发（Vite dev server）
pnpm build    # 产物 dist/（被 BFF 内嵌）
pnpm lint     # 当前为 tsc --noEmit（ESLint 尚未接入）
```
