//! 构建期兜底：`src/admin/mod.rs` 中的 RustEmbed 以 `admin-ui/dist` 为编译期强依赖，
//! 干净克隆（尚未构建管理端）时该目录不存在会直接导致编译失败。
//!
//! 这里在缺失时生成一个最小占位页，保证「克隆即可编译 / 运行」；
//! 在 `admin-ui/` 执行 `pnpm build` 生成真实产物后重新编译即可覆盖占位页。

use std::path::Path;

const PLACEHOLDER_HTML: &str = r#"<!doctype html>
<html lang="zh">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Admin UI 未构建</title>
  <style>
    body { font-family: system-ui, -apple-system, "Segoe UI", sans-serif; max-width: 42rem; margin: 4rem auto; padding: 0 1rem; line-height: 1.7; color: #1f2328; }
    code, pre { background: #f6f8fa; border-radius: 6px; padding: .2rem .4rem; }
    pre { padding: .8rem 1rem; overflow-x: auto; }
    hr { margin: 2rem 0; border: none; border-top: 1px solid #d0d7de; }
    .muted { color: #57606a; font-size: .9rem; }
  </style>
</head>
<body>
  <h1>管理端 UI 未构建</h1>
  <p>这是编译期生成的占位页。要启用完整管理端，请在 <code>admin-ui/</code> 目录执行：</p>
  <pre>pnpm install
pnpm build</pre>
  <p>然后重新编译（<code>cargo build</code> / <code>make build</code>）。</p>
  <hr />
  <p><strong>Admin UI not built.</strong> This placeholder was embedded at compile time. Run
  <code>pnpm install &amp;&amp; pnpm build</code> in <code>admin-ui/</code>, then rebuild the BFF.</p>
  <p class="muted">See CONTRIBUTING.md for the full development setup.</p>
</body>
</html>
"#;

fn main() {
    let dist = Path::new("admin-ui/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(&index, PLACEHOLDER_HTML);
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=admin-ui/dist");
}
