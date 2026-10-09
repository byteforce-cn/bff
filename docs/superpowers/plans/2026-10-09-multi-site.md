# BFF 多站点（Multi-site）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 BFF 从单站点进程演进为“单进程、多监听端口、共享 IdP 与跨站点 SSO”的组织内统一 SPA Web Server，P1 全部落地，legacy 单站点配置零行为变更。

**Architecture:** 配置层新增 `sites[]`（站点）与 `session_profiles`（会话策略），无 `sites` 时合成名为 `default` 的 legacy 站点；运行时以启动期构建的 `SiteHandle`（静态：端口/session layer）+ 每请求解析的 `SiteView`（动态：Host 白名单/public_base_url/spa/provider 绑定/安全头）组成显式 `SiteCtx`，沿 `dispatch → proxy / token_exchange / pipeline / ws` 传递；激活 `ArcSwap` 配置替换统一走 `structural_diff` 判定，启动物化字段变更拒绝热应用并返回 `requires_restart`。

**Tech Stack:** Rust 1.93 / axum 0.7 / tower-sessions 0.12（`SessionManagerLayer::with_domain`）/ metrics 0.23 / serde + figment / tokio；管理台 React 19 + TS（Vite）。

**Spec:** `docs/multi-site-design.md`（v0.4 定稿；§ 引用均指该文件）

## Global Constraints

- 门禁命令：`cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`（`make check`）。测试前若 `admin-ui/dist` 不存在先 `make ui-build`（`build.rs` 有占位页，但 CI 正常构建 UI）。
- 不新增第三方依赖；复用现有 `axum` / `tower-sessions` / `metrics` / `futures` / `url` / `serde` / `humantime_serde`。
- legacy 行为冻结：无 `sites` 的配置启动后行为与升级前完全一致（含多 provider 无 `?provider=` → 400、Host 校验仅 OIDC 路径或 `server.enforce_host` opt-in）。
- 配置字段只增不改名；新增字段一律 `#[serde(default, skip_serializing_if = ...)]`，保证 `/admin/api/config/export` 对 legacy 配置的输出与现状一致。
- 所有校验错误信息必须包含字段路径（如 `sites[0].port`、`session_profiles[isolated].cookie_name`）；prod 判定通过 `AppConfig::validate_with_env(true)` 注入，测试不修改 `BFF_ENV` 环境变量（并行安全）。
- 站点名匹配 `[a-z0-9-]+`，保留名 `admin` 禁止；profile 名匹配 `[a-z0-9-]+` 且不得为 `default`。
- 中文注释与文档风格与仓库现有代码一致；临时文件按 `AGENTS.md` 放 `/tmp/bff-*`。
- 提交信息用 `feat(multi-site): ...` / `test(multi-site): ...` / `docs(multi-site): ...`；每个 Task 结束前必须全量 `cargo test --all-features` 绿。

## 模块地图（实现落点）

| 文件 | 职责 |
|---|---|
| `src/config.rs` | `SiteConfig` / `SessionProfileOverride` / `ResolvedSite` / `ResolvedSessionProfile`、归一化函数、`effective_sites()`、`resolved_session_profiles()`、全部 §5.4 校验 |
| `src/config_fingerprint.rs`（新） | `structural_diff` / `hot_diff` / `ConfigDiff`，集中构造，禁止散落比较 |
| `src/site.rs`（新） | `SiteHandle` / `SiteView` / `SiteCtx` / `PrebuiltSecurityHeaders`、站点级 current_provider/tokens 与 legacy 键迁移 |
| `src/state.rs` | `session_layers`、`site_views`、`site_handles()`、`apply_config()`（结构差异门禁） |
| `src/server/route_dispatcher.rs` | `match_route(routes, site, ...)` 过滤/优先级；`dispatch(SiteCtx)` |
| `src/oidc/handlers.rs` | 站点感知 `select_provider` / `base_url_from` / login/callback/logout / refresh |
| `src/middleware/token_refresh.rs`、`src/server/{proxy,token_exchange}.rs` | SiteCtx 显式传参 |
| `src/server/business.rs` | `build_site_router` / `build_business_router`（legacy 包装）/ 站点级 SPA、`/api/session`、WS、指标 |
| `src/middleware/host_validation.rs`（新） | 421 Host 校验（§6.3） |
| `src/server/serve.rs`（新） | 多 listener 编排 + 可注入 shutdown（§12 listener 测试） |
| `src/admin/runtime_api.rs`、`src/admin/config_api.rs`、`src/admin/mod.rs` | `SessionInfo` additive、`GET /admin/api/sites`、import `requires_restart` 响应 |
| `admin-ui/src/{types/index.ts,lib/api.ts,pages/Sessions.tsx}` | 站点列 + 模拟登录站点下拉 + 全站踢出提示 |
| `tests/common/mod.rs`、`tests/test_multi_site_*.rs` | 多 router 夹具与集成用例 |
| `deploy/{k8s,multi-site}/`、`docs/*.md` | 交付与文档 |

## Review Focus

以下 5 类输入/失败模式在 spec 中未必逐条写明，但最可能伤害使用者；每个 Task 的测试必须显式钉住对应行为（在本节逐条落实到所属 Task）：

1. legacy 配置里出现 `routes[].sites` 注解（无 `sites` 定义）→ 必须启动失败而不是静默把路由变成不可命中（Task 3）。
2. 运行中导入把某站点从 `sites` 删除 → 必须返回 `requires_restart` 且旧配置/旧 listener 保持可用（Task 7）。
3. `public_base_url` 存在时 loopback Host（`localhost`）访问非豁免路径 → 421；未配置时 loopback 放行（Task 12）。
4. 同一站点绑定两个 `callback_path` 相同的 provider → 启动失败；带 `?provider=` 的越站请求 → 400 且不回退默认（Task 4/Task 13）。
5. `session_profiles` 中显式 `cookie_domain: ""`（强制 host-only）与缺省继承的区分 → 被多主机站点引用时按空处理，prod 启动失败（Task 2）。

---

### Task 1: 配置模型：`sites` / `session_profiles` / 安全头覆盖 / 归一化 / legacy 合成

**Files:**
- Modify: `src/config.rs`
- Modify: `src/lib.rs`（仅当后续 Task 新增模块时；本 Task 不需要）
- Test: `src/config.rs` 内 `#[cfg(test)] mod multi_site_config_tests`
- Modify: `tests/test_route_dispatch.rs:23-39`、`tests/test_full_proxy.rs`、`tests/test_token_refresh.rs`、`tests/test_token_exchange.rs`、`tests/test_config_persistence.rs`、`tests/test_oidc_flow.rs`、`tests/test_ws_tunnel.rs`、`src/server/route_dispatcher.rs:369-381`（所有 `RouteDef { ... }` 字面量加 `sites: vec![]`）

**Interfaces:**
- Consumes: 现有 `AppConfig` / `ServerConfig` / `SessionConfig` / `OidcProviderConfig` / `RouteDef` / `SpaConfig` / `SecurityHeadersConfig`。
- Produces（后续 Task 依赖的精确签名）：
  - `pub struct SiteConfig { name: String, port: u16, bind: String, server_names: Vec<String>, public_base_url: Option<String>, session_profile: String, spa: Option<SpaConfig>, oidc: SiteOidcConfig, logout_scope: LogoutScope, security_headers: Option<SiteSecurityHeadersOverride> }`
  - `pub struct SiteOidcConfig { default_provider: String, allowed_providers: Option<Vec<String>> }`
  - `pub enum LogoutScope { Global, Site }`（`Default = Global`，serde `rename_all = "lowercase"`）
  - `pub struct SessionProfileOverride { cookie_name: Option<String>, cookie_domain: Option<String>, secure: Option<bool>, http_only: Option<bool>, same_site: Option<String>, ttl: Option<Duration>, allow_unmanaged_subdomains: Option<bool> }`
  - `pub struct SiteSecurityHeadersOverride { content_security_policy: Option<String>, x_frame_options: Option<String>, x_content_type_options: Option<String>, hsts_max_age: Option<u32>, referrer_policy: Option<String> }`
  - `pub struct ResolvedSessionProfile { name, cookie_name, cookie_domain: Option<String>, secure, http_only, same_site: String, ttl: Option<Duration>, allow_unmanaged_subdomains: bool }`
  - `pub struct ResolvedSite { name, port, bind, server_names: Vec<String>, public_base_url: Option<String>, public_host: Option<String>, allowed_hosts: Vec<String>, spa_dir: String, session_profile, default_provider, allowed_providers: Vec<String>, logout_scope, security_headers: Option<SiteSecurityHeadersOverride>, legacy: bool }`
  - `pub fn normalize_host(raw: &str) -> Option<String>`：trim + 小写 + 去尾点 + `[::1]` 去方括号；含 `://`、`/`、`*`、`:`（端口）、空白/控制字符或空 → `None`。
  - `pub fn normalize_cookie_domain(raw: &str) -> Option<String>`：小写、去前导点；空串 → `Some("")`；非法字符 → `None`。
  - `pub fn cookie_domain_matches(domain: &str, host: &str) -> bool`：`domain` 非空且 `host == domain || host.ends_with(&format!(".{domain}"))`。
  - `pub fn normalize_public_base_url(raw: &str) -> anyhow::Result<(String, String)>`：返回 `(去尾斜杠 URL, 归一化 host)`；http(s)、必须有主机、path 仅允许空或 `/`、无 query/fragment。
  - `impl AppConfig { pub fn effective_sites(&self) -> Vec<ResolvedSite>; pub fn resolved_session_profiles(&self) -> HashMap<String, ResolvedSessionProfile>; }`
- 新增字段：`AppConfig.sites: Vec<SiteConfig>`、`AppConfig.session_profiles: HashMap<String, SessionProfileOverride>`、`ServerConfig.enforce_host: bool`（默认 false）、`SessionConfig.cookie_domain: Option<String>`、`SessionConfig.allow_unmanaged_subdomains: bool`（默认 false）、`OidcProviderConfig.shared_across_sites: bool`（默认 false）、`RouteDef.sites: Vec<String>`（`#[serde(default, skip_serializing_if = "Vec::is_empty")]`）。

- [ ] **Step 1: 写失败测试（解析 + legacy 合成 + 归一化）**

```rust
#[test]
fn parses_explicit_multisite_and_resolves_paths() {
    let cfg: AppConfig = serde_yaml::from_str(MULTISITE_YAML).unwrap();
    let sites = cfg.effective_sites();
    assert_eq!(sites.len(), 2);
    assert_eq!(sites[0].name, "app1");
    assert_eq!(sites[0].port, 8081);
    assert_eq!(sites[0].allowed_hosts, vec!["app1.example.com"]);
    assert_eq!(sites[0].session_profile, "default");
    assert_eq!(sites[0].spa_dir, "apps/app1/dist");
    assert_eq!(sites[0].allowed_providers, vec!["app1"]);
    assert!(!sites[0].legacy);
}

#[test]
fn legacy_config_synthesizes_default_site() {
    let cfg = AppConfig::default();
    let sites = cfg.effective_sites();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].name, "default");
    assert_eq!(sites[0].port, cfg.server.business_port);
    assert!(sites[0].legacy);
}

#[test]
fn session_profile_inheritance_and_explicit_empty_domain() {
    let cfg: AppConfig = serde_yaml::from_str(r#"
session:
  cookie_name: "A"
  cookie_domain: ".Example.com"
  secure: true
  same_site: "Lax"
session_profiles:
  isolated:
    cookie_name: "B"
    cookie_domain: ""
"#).unwrap();
    let p = cfg.resolved_session_profiles();
    assert_eq!(p["default"].cookie_domain.as_deref(), Some("example.com"));
    assert_eq!(p["isolated"].cookie_domain, None); // 显式空串 = 强制 host-only
    assert_eq!(p["isolated"].secure, true);       // 继承顶层
}
```

`MULTISITE_YAML` 用 §5.1/附录 B 的两站点样例（含 `server_names`、`public_base_url`、`session` 的 `cookie_domain: ".example.com"`、两个 provider 带 `shared_across_sites: false`）。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test multi_site_config_tests -- --nocapture`
Expected: 编译失败（`SiteConfig`、`effective_sites` 不存在）。

- [ ] **Step 3: 实现类型与函数**

在 `src/config.rs` 追加类型（字段可见性 `pub`），实现归一化函数；`effective_sites()` 按 §5.5 合成规则（`spa_dir = site.spa.map(dir).unwrap_or(self.spa.dir.clone())`；`allowed_hosts = server_names ∪ {public_host}`；legacy 用 `trusted_hosts` 作 `server_names`、`allowed_providers = 全部 provider.id`、`default_provider = providers[0].id`（无 provider 时为 `""`））。`resolved_session_profiles()` 先构造 `default`（顶层 `session`），再对 `session_profiles` 逐字段继承；`cookie_domain` 用 `Option<String>` 表达：override 显式 `Some("")` → `None`（host-only）；override `None` → 继承顶层；顶层缺省 → `None`；非空值经 `normalize_cookie_domain`。
把全部 `RouteDef { ... }` 字面量补 `sites: vec![]`（用 `rg -n 'RouteDef \{' src tests` 定位）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --all-features`
Expected: 全绿（包含既有 OIDC/proxy/route 测试——证明新增字段未破坏行为）。

- [ ] **Step 5: 提交**

```bash
git add src/config.rs tests src/server/route_dispatcher.rs
git commit -m "feat(multi-site): add sites/session_profiles config model with legacy synthesis"
```

---

### Task 2: 校验（一）：session profile 卫生与 Cookie 策略（§5.4 第 3–5 条）

**Files:**
- Modify: `src/config.rs`（`validate` 重构 + `validate_multi_site` 分支）
- Test: `src/config.rs` 内 `mod session_profile_validation_tests`

**Interfaces:**
- Consumes: Task 1 的 `resolved_session_profiles()` / `effective_sites()` / `normalize_cookie_domain` / `cookie_domain_matches`。
- Produces:
  - `pub fn validate(&self) -> anyhow::Result<()>`（委托 `self.validate_with_env(is_prod_env())`）
  - `pub fn validate_with_env(&self, is_prod: bool) -> anyhow::Result<()>`（现有全部校验迁移进此函数；新增站点/profile 校验分支）
  - `pub(crate) fn is_prod_env() -> bool`
- 测试辅助（写在测试 mod 内）：`fn expect_err(yaml: &str, need: &str)`、`fn expect_ok(yaml: &str)`、`fn expect_prod_err(yaml: &str, need: &str)`。

- [ ] **Step 1: 写失败测试（每条规则正向 + 反向）**

覆盖：
1. profile 名 `[a-z0-9-]+`、不得 `default` → 错误含 `session_profiles[Default].`（键名自定，断言含 `session_profiles` 与名字）。
2. `cookie_name` 跨 profile 全局唯一：`isolated.cookie_name = "BFF_SESSION"`（与 default 相同）→ 错误含 `session_profiles[isolated].cookie_name`。
3. `same_site` 仅 `Strict|Lax|None`；`"None"` + `secure: false` → 错误含 `same_site`。
4. prod：任一 profile `secure != true` → 错误含 profile 名与 `secure`。
5. `cookie_domain` 非空必须 domain-match 引用它的所有站点主机：profile 只被 `app1.example.com` 引用而配 `.other.example` → 错误含 `cookie_domain`；`example.com` 同时匹配 `app1.example.com` → 通过。
6. 被 ≥2 个不同主机站点引用的 profile 且 `cookie_domain` 为空：`validate_with_env(true)` → 错误；`validate_with_env(false)` → 通过（dev warn，断言返回 Ok）。
7. prod：`cookie_domain` 非空且 `allow_unmanaged_subdomains != true` → 错误含 `allow_unmanaged_subdomains`；显式 `true` → 通过；dev 未确认 → 通过。

```rust
fn expect_prod_err(yaml: &str, need: &str) {
    let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
    let err = cfg.validate_with_env(true).expect_err("prod 应校验失败").to_string();
    assert!(err.contains(need), "错误应含 {need}，实际: {err}");
}
#[test]
fn shared_profile_without_domain_fails_prod() {
    expect_prod_err(SHARED_PROFILE_NO_DOMAIN_YAML, "cookie_domain");
    let cfg: AppConfig = serde_yaml::from_str(SHARED_PROFILE_NO_DOMAIN_YAML).unwrap();
    assert!(cfg.validate_with_env(false).is_ok(), "dev 仅告警");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test session_profile_validation_tests`
Expected: 编译失败（`validate_with_env` 不存在）。

- [ ] **Step 3: 实现**

`validate()` 改为 `self.validate_with_env(is_prod_env())`；把现有 `validate` 体内的 `is_prod` 局部读取替换为入参。追加 `validate_session_profiles(&self, is_prod, sites: &[ResolvedSite]) -> anyhow::Result<()>`：
- 规则 3（名字/cookie_name 唯一/same_site）无条件执行；
- 规则 4（domain-match）：仅对“被站点引用”的 profile；用 `ResolvedSite.allowed_hosts` 做正向匹配；至少一个 host 不匹配即错误；
- 规则 5：按引用该 profile 的**站点数**（≥2 且主机集合去重>1）判定；prod 失败、dev `tracing::warn!`；
- 规则 5（allow_unmanaged）：`cookie_domain` 非空且非 prod 时 warn，prod 时 error；
- 递归调用顺序：先做现有 legacy 校验（保持一致），再 `validate_session_profiles`，再 Task 3/4 的站点校验（后续加入）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add src/config.rs
git commit -m "feat(multi-site): validate session profiles and cookie policy"
```

---

### Task 3: 校验（二）：站点字段与 Host 白名单（§5.4 第 1/2/6/7/11 条 + §5.3 重复路由告警）

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` 内 `mod site_validation_tests`

**Interfaces:**
- Consumes: Task 1/2 的类型与辅助。
- Produces: `fn validate_sites(&self, is_prod: bool, sites: &[ResolvedSite]) -> anyhow::Result<()>`。

- [ ] **Step 1: 写失败测试**

覆盖：
1. 站点名：空/大写/下划线 → 错误含 `sites[0].name`；重名 → 错误含 `sites[1].name`；`admin` → 错误含 `sites[0].name`。
2. 端口：重复、`0`、等于 `admin_port` → 错误含 `sites[...].port`。
3. `bind` 必须是可解析 IP（如 `"not-an-ip"` → 错误含 `.bind`）。
4. 显式多站点 + prod：缺 `public_base_url` → 错误含 `sites[0].public_base_url`；`server_names` 未包含 public 主机 → 错误含 `server_names`；两站点归一化后 public 相同 → 错误含 `public_base_url`。
5. `server.public_base_url` / `server.trusted_hosts` 在显式多站点下非空 → 错误含 `server.public_base_url`（dead config）；`business_port != 8080` → warn（用 `tracing_test` 不便，改为断言不报错即可，warn 由眼检/日志）。
6. `server_names`：含 `https://x`、`x:8080`、`*`、`a/b` → 错误含 `server_names`；大小写与尾点归一（`"App1.EXAMPLE.com."` 等价于 `"app1.example.com"`，与 public 主机去重后 `allowed_hosts.len()==1`）。
7. 多站点 prod 两站点解析到同一 `spa.dir` → 返回 Ok（warn）。
8. legacy 冻结：无 `sites` 且 `routes[0].sites` 非空 → 错误含 `routes[0].sites`（Review Focus #1）。
9. §5.3 重复告警：同一站点可命中的两条完全相同规格（同 path + 同 methods + 同 sites）→ `validate_with_env(false)` 返回 Ok（warn，不 fail）。

```rust
#[test]
fn rejects_route_sites_without_sites_definition() {
    expect_err(r#"
routes:
  - path: "/api/x"
    sites: ["app1"]
    type: static
    config: { status: 200 }
"#, "routes[0].sites");
}
#[test]
fn normalizes_server_names_case_and_trailing_dot() {
    let cfg: AppConfig = serde_yaml::from_str(MULTIDOT_YAML).unwrap();
    let s = &cfg.effective_sites()[0];
    assert_eq!(s.allowed_hosts, vec!["app1.example.com"]);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test site_validation_tests`
Expected: 失败。

- [ ] **Step 3: 实现**

`validate_sites` 按上表实现；`server_names` 校验复用 `normalize_host`（`None` → error），站点内去重；`effective_sites()` 已负责归一化与并集。重复路由告警函数 `fn warn_duplicate_routes(&self, sites: &[String])`：对每条 `RouteDef` 计算 `(normalize_path, sorted(methods), sorted(sites or all))`，同键 >1 时 `tracing::warn!`，含路由索引。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/config.rs
git commit -m "feat(multi-site): validate sites, host allowlist and dead config"
```

---

### Task 4: 校验（三）：provider 绑定、回调路径、令牌隔离（§5.4 第 8/9/10 条）

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` 内 `mod provider_binding_validation_tests`

**Interfaces:**
- Consumes: Task 1–3。
- Produces: `fn validate_site_providers(&self, sites: &[ResolvedSite]) -> anyhow::Result<()>`。

- [ ] **Step 1: 写失败测试**

覆盖：
1. `default_provider` 不在 `oidc.providers` → 错误含 `sites[0].oidc.default_provider`。
2. `default_provider ∉ allowed_providers` → 错误含 `allowed_providers`。
3. `routes[i].sites` 引用未定义站点 → 错误含 `routes[0].sites`（与 Task 3 的 legacy 分支区分：此处 sites 非空）。
4. 同一站点绑定的两个 provider `callback_path` 相同 → 错误含 `callback_path`。
5. 不同站点 allowed_providers 有交集且 provider 未设 `shared_across_sites: true` → 错误含 `shared_across_sites`；显式 `true` → 通过（Review Focus #4 的启动部分）。
6. 同一站点允许绑定同一 provider 多次？`allowed_providers` 去重后不含默认/未知 → 错误。

```rust
#[test]
fn rejects_shared_provider_without_opt_in() {
    expect_err(MULTISITE_SHARED_PROVIDER_YAML, "shared_across_sites");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test provider_binding_validation_tests`

- [ ] **Step 3: 实现**

按站点遍历：provider 存在性、默认 ∈ allowed、allowed 去重与存在性；每站点 `callback_path` 集合唯一（`HashSet`）。跨站点：按 `(session_profile, provider_id)` 统计引用站点数 >1 → provider 必须 `shared_across_sites`。`routes[].sites` 校验只在 `self.sites` 非空时执行（legacy 分支已在 Task 3 处理）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/config.rs
git commit -m "feat(multi-site): validate provider bindings and token isolation"
```

---

### Task 5: 结构指纹与差异（§5.6）

**Files:**
- Create: `src/config_fingerprint.rs`
- Modify: `src/lib.rs`（`pub mod config_fingerprint;`）
- Test: `src/config_fingerprint.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `AppConfig`、`effective_sites()`、`resolved_session_profiles()`。
- Produces:
  - `pub struct ConfigDiff { pub hot_applied: Vec<String>, pub requires_restart: Vec<String> }`
  - `pub fn requires_restart_fields(old: &AppConfig, new: &AppConfig) -> Vec<String>`：结构字段路径（见下），顺序稳定。
  - `pub fn hot_applied_fields(old: &AppConfig, new: &AppConfig) -> Vec<String>`：粗粒度组名。
  - `pub fn diff(old: &AppConfig, new: &AppConfig) -> ConfigDiff`。
- 结构字段（每个都必须有单测）：`server.admin_port`；`provider.{session_store,cache,lock,redis_url}`；`http_client.*`（全部字段）；`rate_limit.{per_second,burst_size,skip_path_prefixes}`；`cors.{permissive,allowed_origins}`；`body_limit.max_bytes`；`circuit_breaker.*`；`scripting.max_duration`；`telemetry.*`；`persistence.enabled`；`session.gc_interval`；`session_profiles[<name>].{cookie_name,cookie_domain,secure,http_only,same_site,ttl}`（含 default，用解析后值）；`sites[<name>].{port,bind,session_profile}` 与站点增删（`sites[<name>]`）；`oidc.providers.callback_paths`（排序集合）。
- `hot_applied` 组名：`routes`、`oidc.providers`、`sites.view`、`persistence`、`health`、`websocket`、`token_refresh`、`admin`、`spa`。

- [ ] **Step 1: 写失败测试（表驱动 + 顺序不敏感）**

```rust
fn changed(mutate: impl FnOnce(&mut AppConfig)) -> (AppConfig, AppConfig) {
    let old = sample_multisite();
    let mut new = old.clone();
    mutate(&mut new);
    (old, new)
}
#[test]
fn each_structural_field_is_detected() {
    let cases: Vec<(&str, Box<dyn FnOnce(&mut AppConfig)>)> = vec![
        ("server.admin_port", Box::new(|c| c.server.admin_port = 9443)),
        ("sites[app1].port", Box::new(|c| c.sites[0].port = 9081)),
        ("session_profiles[default].cookie_name", Box::new(|c| c.session.cookie_name = "X".into())),
        ("oidc.providers.callback_paths", Box::new(|c| c.oidc.providers[0].callback_path = "/cb".into())),
        // ... 每个结构字段一行
    ];
    for (field, m) in cases {
        let (old, new) = changed(m);
        let fields = requires_restart_fields(&old, &new);
        assert!(fields.iter().any(|f| f == field), "{field} 未检出: {fields:?}");
    }
}
#[test]
fn order_insensitive_and_resolved_profiles() {
    let mut a = sample_multisite();
    let mut b = sample_multisite();
    b.sites.reverse();
    b.oidc.providers.reverse();
    assert!(requires_restart_fields(&a, &b).is_empty());
    a.session_profiles.insert("x".into(), /* 与 default 完全相同 */ Default::default());
    // 新增 profile 改变指纹（即使无站点引用）
    assert!(!requires_restart_fields(&a, &b).is_empty());
}
#[test]
fn callback_path_swap_between_providers_is_hot() {
    let (old, mut new) = changed(|_| {});
    let p0 = new.oidc.providers[0].callback_path.clone();
    let p1 = new.oidc.providers[1].callback_path.clone();
    new.oidc.providers[0].callback_path = p1;
    new.oidc.providers[1].callback_path = p0;
    // 集合内互换不改变启动注册的路径集合 → 热生效（§5.6）
    assert!(requires_restart_fields(&old, &new).is_empty());
}
#[test]
fn hot_fields_not_structural() {
    let (old, mut new) = changed(|c| c.routes.push(/* 任意 static 路由 */));
    new.oidc.providers[0].client_id = "new".into();
    let fields = requires_restart_fields(&old, &new);
    assert!(fields.is_empty(), "热字段不应要求重启: {fields:?}");
    assert!(hot_applied_fields(&old, &new).contains(&"routes".to_string()));
    assert!(hot_applied_fields(&old, &new).contains(&"oidc.providers".to_string()));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test config_fingerprint`
Expected: 编译失败。

- [ ] **Step 3: 实现**

用手写分组比较（不要 `Debug` 字符串化整块配置）：对每个结构组逐字段比较并 push 字段路径；sites/profiles 以名字为键对齐，遍历 `BTreeMap` 保证顺序稳定；`callback_paths` 取排序去重集合；站点 `sites[<name>].port` 等路径精确到字段。`hot_applied_fields` 粗粒度比较：路由（`routes` 不等）、provider 内容（除 callback_path 集合外的字段）、站点视图字段（server_names/public_base_url/spa/profile 绑定/security_headers/logout_scope）、`persistence.path/watch_interval`、`health`、`websocket`、`token_refresh.skip_prefixes`、`admin`（除 admin_port）、legacy `spa.dir`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/config_fingerprint.rs src/lib.rs
git commit -m "feat(multi-site): add structural fingerprint and config diff"
```

---

### Task 6: 站点运行时类型：`SiteHandle` / `SiteView` / `SiteCtx` / 安全头预构建 / 站点级令牌解析

**Files:**
- Create: `src/site.rs`
- Modify: `src/lib.rs`（`pub mod site;`）
- Test: `src/site.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `AppConfig` / `ResolvedSite` / `ResolvedSessionProfile` / `StoredTokens` / `session_key` / `SessionManagerLayer<DynSessionStore>`。
- Produces:
  - `pub struct SiteHandle { pub name: String, pub port: u16, pub bind: String, pub session_profile: String, pub session_layer: SessionManagerLayer<crate::provider::session::DynSessionStore>, pub legacy: bool }`
  - `pub struct SiteView { pub name, pub port, pub server_names: Vec<String>, pub allowed_hosts: Vec<String>, pub public_base_url: Option<String>, pub spa_dir: String, pub default_provider: String, pub allowed_providers: Vec<String>, pub logout_scope: LogoutScope, pub security_headers: Arc<PrebuiltSecurityHeaders>, pub legacy: bool }`
  - `pub struct SiteCtx<'a> { pub handle: &'a SiteHandle, pub view: Arc<SiteView> }`
  - `pub struct PrebuiltSecurityHeaders { pub csp_default: Option<HeaderValue>, pub csp_overrides: Vec<(String, HeaderValue)>, pub x_frame_options: Option<HeaderValue>, pub x_content_type_options: Option<HeaderValue>, pub hsts: Option<HeaderValue>, pub referrer_policy: Option<HeaderValue> }`
  - `impl PrebuiltSecurityHeaders { pub fn build(global: &SecurityHeadersConfig, ov: Option<&SiteSecurityHeadersOverride>) -> anyhow::Result<Self>; pub fn apply(&self, path: &str, headers: &mut HeaderMap); }`
  - `impl SiteView { pub fn from_resolved(site: &ResolvedSite, security_headers: Arc<PrebuiltSecurityHeaders>) -> Self; pub async fn current_provider(&self, session: &Session) -> Option<String>; pub async fn current_tokens(&self, session: &Session) -> Option<StoredTokens>; pub async fn current_access_token(&self, session: &Session) -> Option<String>; pub fn canonical_base_url(&self) -> String; pub fn provider_key(&self) -> String; }`
- `current_provider` 统一逻辑（§7.2）：读 `oidc:{site}:current_provider`（`provider_key()`）→ 有效且 ∈ `allowed_providers` 且对应 `session_key(provider)` 有 token → 返回；否则尝试 `default_provider` 的 token，有则写回 `provider_key` 并返回；仍无 → `None`。`self.legacy == true` 时额外读旧键 `oidc:current_provider`：有效且 ∈ allowed 且有 token → 迁移（写 `oidc:default:current_provider`、删旧键）后返回。
- `PrebuiltSecurityHeaders::build`：合并语义——`ov.content_security_policy` 为 `Some` 时整体替换全局 CSP 且 `csp_overrides = vec![]`；否则继承全局 CSP 与 `csp_overrides`（按 `path_prefix` 长度降序预解析）；其余字段逐字段 `unwrap_or(global)`；空串字段视为“不发送”（`None`）；`HeaderValue` 解析失败返回 `Err`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn current_provider_falls_back_and_writes_back() {
    let view = view_with_allowed(&["p1", "p2"], "p2");
    let session = Session::new(None, Arc::new(DynSessionStore::new(store())), None);
    session.insert(&crate::oidc::tokens::session_key("p2"), tokens("p2")).await.unwrap();
    assert_eq!(view.current_provider(&session).await.as_deref(), Some("p2"));
    let written: Option<String> = session.get(&view.provider_key()).await.unwrap();
    assert_eq!(written.as_deref(), Some("p2"));
}
#[tokio::test]
async fn provider_outside_whitelist_is_ignored() {
    let view = view_with_allowed(&["p1"], "p1");
    let session = /* 写入 oidc:{site}:current_provider = "p9" 与 p9 token */;
    assert!(view.current_tokens(&session).await.is_none());
}
#[tokio::test]
async fn legacy_key_is_migrated_only_in_legacy_mode() {
    // legacy=true：写旧键 + token → current_provider 返回并删除旧键、写新键
    // legacy=false：同样输入 → 返回 None（不读旧键）
}
#[test]
fn prebuilt_headers_csp_replaces_overrides_only_when_specified() {
    let global = SecurityHeadersConfig { csp_overrides: vec![CspOverrideConfig { .. }], ..Default::default() };
    let inherit = PrebuiltSecurityHeaders::build(&global, None).unwrap();
    assert_eq!(inherit.csp_overrides.len(), 1);
    let ov = SiteSecurityHeadersOverride { content_security_policy: Some("default-src 'none'".into()), ..Default::default() };
    let replaced = PrebuiltSecurityHeaders::build(&global, Some(&ov)).unwrap();
    assert!(replaced.csp_overrides.is_empty());
    assert_eq!(replaced.csp_default.unwrap().to_str().unwrap(), "default-src 'none'");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test site::`
Expected: 编译失败。

- [ ] **Step 3: 实现**

`src/site.rs` + `pub mod site;`。`PrebuiltSecurityHeaders::apply` 按路径前缀（降序、`starts_with`）选 CSP；空值跳过；`hsts` 以 `max-age=N` 形式预构建。`current_*` 的 session 读写用 `session.get/insert/remove_value`（`tower_sessions::Session`）。不依赖 `AppState`（避免循环依赖）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/site.rs src/lib.rs
git commit -m "feat(multi-site): add site runtime types and prebuilt security headers"
```

---

### Task 7: `AppState` 装配：session layer map、site view map、`site_handles()`、`apply_config` 结构门禁

**Files:**
- Modify: `src/state.rs`
- Modify: `src/provider/session.rs`（`build_layer` 入参换成 `&ResolvedSessionProfile`，支持 `cookie_domain` → `with_domain`）
- Modify: `src/admin/config_api.rs`（import 响应 + 其他写接口错误映射）
- Test: `src/state.rs` 内 `mod apply_config_tests`；`tests/test_multi_site_config_import.rs`（新）

**Interfaces:**
- Consumes: Task 5 的 `config_fingerprint::diff`；Task 6 的 `SiteHandle`/`SiteView`。
- Produces:
  - `pub enum ConfigApplyError`（`#[derive(Debug)]`）`{ Rejected(anyhow::Error), RequiresRestart(config_fingerprint::ConfigDiff) }`
  - `impl AppState { pub fn site_view(&self, name: &str) -> Option<Arc<SiteView>>; pub fn site_handles(&self) -> anyhow::Result<Vec<Arc<SiteHandle>>>; pub async fn apply_config(&self, cfg: AppConfig) -> Result<ConfigDiff, ConfigApplyError>; pub async fn apply_watched_config(&self, cfg: AppConfig, file_hash: String) -> anyhow::Result<()>; }`
  - `pub session_layers: HashMap<String, SessionManagerLayer<DynSessionStore>>`（`AppState` 字段）
  - `site_views: Arc<ArcSwap<HashMap<String, Arc<SiteView>>>>`（私有字段 + `site_view()` 读取；`ArcSwap` 不 Clone，必须套 `Arc`）
- `apply_config`（async）顺序：`cfg.validate()` → bff_secret 检查 → `diff(current, new)` → `requires_restart` 非空则 `Err(RequiresRestart)` → 预构建新 site view map（失败 → `Rejected`）→ `persist_config` → `store` → `site_views.store` → **对 old∪new 的全部 provider id 调 `oidc_clients.invalidate(id).await`**（§5.6 集中失效钩子）→ `Ok(diff)`。`apply_watched_config` 复用同一内部函数但 `persist = false`，结构差异时 `tracing::warn!` 并返回 Err；两者都在成功后完成 provider 缓存失效。
- 删除旧 `replace_config`；更新全部调用点（`rg -n 'replace_config' src tests`），并**删除各 handler 里分散的 `oidc_clients.invalidate` 调用**（`config_api::{import_config, update_provider, delete_provider}` 与 `state::run_config_watcher` 的收尾循环），避免“改了配置但行为不变”的隐性故障。import handler 映射：`Rejected` → 422 `{"error": ...}`；`RequiresRestart(d)` → 200 `{"status":"requires_restart","hot_applied":d.hot_applied,"requires_restart":d.requires_restart}`；成功 → 200 `{"status":"applied","hot_applied":...}`。

- [ ] **Step 1: 写失败测试**

`src/state.rs` 单测：
```rust
#[tokio::test]
async fn apply_config_rejects_structural_change() {
    let state = AppState::new(multisite_cfg()).unwrap();
    let mut next = state.cfg().as_ref().clone();
    next.sites[0].port += 1;
    match state.apply_config(next).await {
        Err(ConfigApplyError::RequiresRestart(d)) => {
            assert!(d.requires_restart.iter().any(|f| f == "sites[app1].port"));
        }
        other => panic!("应 RequiresRestart，实际 {other:?}"),
    }
    assert_eq!(state.cfg().sites[0].port, 8081, "旧配置必须保持");
}
#[tokio::test]
async fn apply_config_rejects_site_removal() {
    let state = AppState::new(multisite_cfg()).unwrap();
    let mut next = state.cfg().as_ref().clone();
    next.sites.remove(1);
    let err = state.apply_config(next).await.unwrap_err();
    assert!(matches!(err, ConfigApplyError::RequiresRestart(_)), "删站点必须要求重启");
}
#[tokio::test]
async fn apply_config_applies_hot_change_and_rebuilds_views() {
    /* 改 routes 后 Ok；site_view("app1") 仍存在，spa_dir 等字段取自新配置 */
}
```
`tests/test_multi_site_config_import.rs`：
```rust
#[tokio::test]
async fn import_returns_requires_restart_and_keeps_old_config() {
    // 启动 admin + 多站点 state；POST 修改 sites[0].port 的完整 YAML
    // 断言 200 + body["status"]=="requires_restart" + requires_restart 含 "sites[app1].port"
    // 且 state.cfg().sites[0].port 未变
}
#[tokio::test]
async fn import_site_removal_requires_restart() {
    // POST 只保留 app1（删除 app2）的 YAML → requires_restart 含 "sites[app2]"，旧配置两份站点仍在
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test apply_config`（编译失败）

- [ ] **Step 3: 实现**

`AppState::new` 末尾构建：`session_layers`（遍历 `resolved_session_profiles()` 调 `build_layer`）；`site_views`（遍历 `effective_sites()` 建 `PrebuiltSecurityHeaders::build` + `SiteView::from_resolved`）。`site_handles()` 按 effective sites 建 `Arc<SiteHandle>`（layer 从 map 取，profile 缺失 → `anyhow::bail!`）。`apply_config` 按上面顺序；`apply_watched_config` 改为调用内部 `apply_inner(cfg, persist=false)` 并保留 `is_own_config_write` 哈希更新与 bff_secret 检查。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`
Expected: 全绿；注意 `tests/test_admin_config_import_export.rs` 现有回环测试仍 200/applied。

- [ ] **Step 5: 提交**

```bash
git add src/state.rs src/provider/session.rs src/admin/config_api.rs tests/test_multi_site_config_import.rs
git commit -m "feat(multi-site): wire site views into AppState and gate structural config changes"
```

---

### Task 8: 路由匹配站点化（§5.3）

**Files:**
- Modify: `src/server/route_dispatcher.rs`（`match_route` 签名 + 测试）
- Modify: `src/server/business.rs`（`fallback_handler`、`ws_upgrade_handler`、`metrics_path_label` 的匹配调用——本 Task 先传 legacy 站点名 `"default"`，Task 9 再换 SiteCtx）

**Interfaces:**
- Produces: `pub fn match_route<'a>(routes: &'a [RouteDef], site: &str, method: &str, path: &str) -> Option<&'a RouteDef>`
- 规则（§5.3）：① `r.sites.is_empty() || r.sites.iter().any(|s| s == site)`；② 段边界最长前缀 + 方法过滤；③ 同长度时 `!r.sites.is_empty()` 优先；④ 再相同按配置顺序 last-wins（`max_by_key` 天然 last-wins）。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn site_filter_and_specialization_priority() {
    let mut global = r("/api/x", &[]);
    let mut app1 = r("/api/x", &[]);
    app1.sites = vec!["app1".into()];
    let routes = vec![global, app1];
    assert_eq!(match_route(&routes, "app1", "GET", "/api/x").unwrap().sites, vec!["app1"]);
    assert!(match_route(&routes, "app2", "GET", "/api/x").unwrap().sites.is_empty());
}
#[test]
fn site_filtered_route_is_invisible_elsewhere() {
    let mut only = r("/api/only", &[]);
    only.sites = vec!["app2".into()];
    assert!(match_route(&[only], "app1", "GET", "/api/only").is_none());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test route_dispatcher`（编译失败：旧签名调用）

- [ ] **Step 3: 实现并更新调用点**

`max_by_key(|r| (r.path.trim_end_matches('/').len(), !r.sites.is_empty()))`；business.rs 三处改传 `"default"` 占位。更新 `route_dispatcher.rs` 内部测试调用签名。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/server/route_dispatcher.rs src/server/business.rs
git commit -m "feat(multi-site): site-filtered route matching with specialization priority"
```

---

### Task 9: OIDC 与令牌链路一次性站点化（§6.1 迁移顺序、§6.2、§7.2/§7.4、§8）

**Files:**
- Modify: `src/oidc/handlers.rs`
- Modify: `src/middleware/token_refresh.rs`
- Modify: `src/server/route_dispatcher.rs`（`dispatch` / `build_context_json`）
- Modify: `src/server/proxy.rs`（`forward_request` / `resolve_auth_token`）
- Modify: `src/server/token_exchange.rs`（`resolve` / `do_exchange_with_retry` / `resolve_token_endpoint`）
- Modify: `src/server/business.rs`（handler 挂 `Extension<Arc<SiteHandle>>`；fallback/ws 构造 `SiteCtx`）
- Test: `src/oidc/handlers.rs` 内单测 + 既有 `tests/test_oidc_flow.rs` / `test_token_refresh.rs` / `test_token_exchange.rs` / `test_full_proxy.rs` / `test_pipeline_auth.rs` / `test_public_base_url.rs` 全绿

> 为什么 9/10 合并：`current_tokens(session)` 等旧单参函数没有 `state`，无法在 handler 之外解析 legacy 视图；把站点上下文一次性贯通才能让每个 Step 结束都保持全量测试绿（§6.1 要求的“分步迁移、每步绿”在本 Task 内以 a/b/c 子步骤执行）。

**Interfaces（最终签名，后续 Task 只依赖这些）:**
- `pub fn select_provider(state: &AppState, site: &SiteView, id: Option<&str>) -> Result<OidcProviderConfig, AppError>`：非 legacy 时 `Some(id)` 必须 ∈ `allowed_providers`，否则 **400**；`None` → `default_provider`。legacy 语义不变（未知 id → 404；无 id 且多 provider → 400）。
- `fn base_url_from(headers: &HeaderMap, site: &SiteView) -> Result<String, AppError>`：`public_base_url` 优先；否则 Host 必须 ∈ `allowed_hosts`（Host 缺失 → 回退 `http://127.0.0.1:{site.port}`）；`public_base_url.is_none()` 时 loopback Host 兜底放行。
- `pub fn canonical_base_url(site: &SiteView) -> String`：`public_base_url` 或 `http://127.0.0.1:{port}`。
- `pub async fn try_refresh(state, site: &SiteView, session, tokens)` / `force_refresh(...)`：`select_provider(state, site, Some(&tokens.provider))`。
- `pub async fn current_tokens(site: &SiteView, session) -> Option<StoredTokens>` / `current_access_token(site, session)`：委托 `SiteView`（旧单参版本删除）。
- `pub async fn dispatch(state: &AppState, site: &SiteCtx<'_>, route, session, req)`；`build_context_json(session, site: &SiteView, mapping)`。
- `proxy::forward_request(state, site: &SiteCtx<'_>, session, route, upstream, req)`；`resolve_auth_token(state, site, session, route)`。
- `token_exchange::resolve(state, site: &SiteView, session, cfg)`；`resolve_token_endpoint` 用 `site.current_tokens`。
- `token_refresh_middleware(State, Extension<Arc<SiteHandle>>, session, req, next)`：`let view = state.site_view(&handle.name)`（取不到 → 500 `{"error":"站点配置缺失"}`），只刷新站点当前 provider。
- `login` / `callback` / `logout` 增加 `Extension<Arc<SiteHandle>>`；callback 写入 `oidc:{site}:current_provider`；`logout`：`Global` = flush + 全部 exchange 缓存 + RP-Initiated（用触发站点选出的 provider 与 id_token_hint）；`Site` = 仅移除 `allowed_providers` 全部 `oidc:{p}:tokens` + `provider_key`，不调 IdP，重定向 `/`，仍清理 exchange 缓存。
- `build_business_router` 取 `state.site_handles()` 中的 legacy `default` handle，`.layer(axum::Extension(handle))` 作为**最外层**（最后调用 `.layer`），保证 TraceLayer/middleware/handler 都能读到。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn multisite_provider_not_in_whitelist_is_400() {
    // SiteView { allowed_providers: vec!["p1"], default_provider: "p1", legacy: false, .. }
    // select_provider(state, &view, Some("p2")) → AppError.status == 400
}
#[test]
fn legacy_multi_provider_without_id_is_400() { /* legacy view + 2 providers + None → 400 */ }
#[test]
fn base_url_uses_public_base_url_and_rejects_unlisted_host() {
    // view.public_base_url = Some("https://a.example") → 任意 Host 都返回它
    // 无 public_base_url + allowed_hosts=["a.example"] + Host=evil → Err(400)
    // 无 public_base_url + Host=localhost → Ok("http://localhost")（dev 兜底）
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --all-features`
Expected: 编译失败（旧签名调用链）。

- [ ] **Step 3: 按 §6.1 顺序实现，每个子步骤后跑全量测试**

- **3a. handler 层**：`handlers.rs` 改为最终签名；`business.rs` 的 OIDC 路由挂 `Extension`（legacy handle），handler 内 `let view = state.site_view(&handle.name).expect("site view")` + `SiteCtx`。Run: `cargo test --all-features`（此步 `route_dispatcher`/`proxy` 仍编译失败 → 必须在同一步把它们的调用点改成 `let view = state.site_view("default")` 的临时代理，才能真正绿）。
- **3b. dispatch/代理链**：`route_dispatcher::dispatch`、`build_context_json`、`proxy`、`token_exchange` 全部接收 `&SiteCtx`/`&SiteView`；`business.rs::fallback_handler`/`ws_upgrade_handler`/`metrics_path_label` 构造并传入。Run: `cargo test --all-features`。
- **3c. 中间件**：`token_refresh_middleware` 用 `Extension` handle + `view.current_tokens`。Run: `cargo test --all-features`。
- **3d. 清理**：删除全部旧单参函数；`rg -n 'current_(access_)?token[s]?\(&?session' src` 只允许出现新签名；`rg -n 'site_view\("default"\)' src` 只允许出现在 `business.rs` 的 legacy 包装与测试。

关键实现点：
```rust
// logout scope 分支（handlers.rs）
match site.logout_scope {
    LogoutScope::Site => {
        for p in &site.allowed_providers {
            session.remove_value(&session_key(p)).await.ok();
        }
        session.remove_value(&site.provider_key()).await.ok();
        clear_session_cache(state, sid).await;
        return Ok(Redirect::to("/").into_response());
    }
    LogoutScope::Global => { /* 现有 flush + RP-Initiated 逻辑 */ }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`
Expected: 全绿（含 mock IdP 登录链路与 token refresh/exchange 回归）。

- [ ] **Step 5: 提交**

```bash
git add src/oidc/handlers.rs src/middleware/token_refresh.rs src/server/route_dispatcher.rs src/server/proxy.rs src/server/token_exchange.rs src/server/business.rs
git commit -m "feat(multi-site): thread SiteCtx through OIDC and token chain"
```

---

### Task 10: 多站点测试夹具与共享会话回归（§7.1/§7.2、§18-7）

**Files:**
- Modify: `tests/common/mod.rs`
- Test: `tests/test_shared_session.rs`（新）

**Interfaces:**
- `tests/common` 新增：
  - `pub fn multisite_config(idp_a: &MockIdp, idp_b: &MockIdp) -> AppConfig`：**dev 语义夹具**——两站点（`app1: 8081`、`app2: 8082`，`server_names: []`、`public_base_url: None`，loopback Host 放行）、共享 `session` profile（`cookie_name = "BFF_SESSION_V2"`、`cookie_domain = ".test"`、`allow_unmanaged_subdomains: true`）、两个 mock provider（`callback_path` 均为 `/auth/callback`，`shared_across_sites: false`）。需要 prod 语义（public_base_url/421/Host 白名单）的用例（Task 12/18）自行基于 `base_config()` 构造并显式设 `server_names`/`public_base_url`。
  - `pub fn write_tokens(session: &tower_sessions::Session, provider: &str) -> StoredTokens` 风格的小工具（复用 `create_session_with_tokens` 的实现思路，可让 `create_session_with_tokens` 内部调用它）。
- 测试用例（不依赖 HTTP router，直接操作 `AppState` + `Session`）：
  1. `site_handles_returns_two_separate_handles`：`state.site_handles()` 长度 2、name/port/session_profile 正确、`legacy == false`；legacy 配置返回 1 个 `legacy == true` 的 `default`。
  2. `same_store_session_is_visible_across_site_views`：用站点 A 的 `Session` 写入 `oidc:pA:tokens` 并 `save()`；用同一 store、同一 id 新建 `Session`，站点 A 视图 `current_tokens` 可见；站点 B 视图（只允许 `pB`，默认 `pB`）不可见。
  3. `legacy_key_migrates_on_first_read`：`common::create_session_with_tokens`（写旧键 `oidc:current_provider`）+ token；`state.site_view("default").current_provider` 返回 provider，旧键被删除，`oidc:default:current_provider` 已写入。
  4. `multisite_config_validates_and_keeps_profiles_isolated`：`AppState::new(multisite_config(...))` 成功；`state.session_layers` 含 `default`；改动两站点 provider 交集后 `AppState::new` 失败（与 Task 4 的校验联动）。

- [ ] **Step 1: 写失败测试**（上述 4 条）

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test test_shared_session`

- [ ] **Step 3: 实现夹具与测试**

`multisite_config` 用 Task 1 的类型直接构造（`AppConfig { sites: vec![...], session: SessionConfig { cookie_name, cookie_domain: Some(".test".into()), allow_unmanaged_subdomains: true, ..Default::default() }, ..base_config() }`，providers 用 `common::mock_provider_cfg` 改 id/callback）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add tests/common/mod.rs tests/test_shared_session.rs
git commit -m "test(multi-site): shared session fixtures and legacy key migration"
```

---

### Task 11: `build_site_router` + 站点级中间件（SPA / 安全头 / session_info / WS / metrics site 标签）

**Files:**
- Modify: `src/server/business.rs`
- Test: `tests/test_multi_site_runtime.rs`（新）、`tests/common/mod.rs`（新增夹具）

**Interfaces:**
- Produces:
  - `pub fn build_site_router(state: AppState, handle: Arc<SiteHandle>) -> anyhow::Result<Router>`
  - `pub fn build_business_router(state: AppState) -> anyhow::Result<Router>`：`cfg.sites` 非空时 `bail!("显式多站点配置请使用 build_site_router")`；否则取 legacy `default` handle 调用 `build_site_router`（现有测试零改动）。
  - `tests/common` 新增：`pub async fn spawn_site(state: &AppState, name: &str) -> String`；`pub fn site_handle(state: &AppState, name: &str) -> Arc<SiteHandle>`（`multisite_config` 由 Task 10 提供，直接复用）。
- router 结构：路由与 handler 不变；`Extension<Arc<SiteHandle>>` 作为最外层；session layer 用 `handle.session_layer.clone()`；security headers 中间件改为读 `SiteCtx` 的 `view.security_headers.apply(path, ...)`；metrics 中间件加 `"site" => handle.name` 且 `/live`、`/ready` 不计入业务指标；`session_info` 返回 `{logged_in, provider?}`（按站点 current provider）；`serve_spa` 用 `view.spa_dir`；`run_pipeline` 鉴权用站点 token；`ws_upgrade_handler` 用站点 token。`build_site_router` 里构造 legacy/单站点上下文用 `SiteCtx { handle: handle.as_ref(), view }`（`handle` 是 `Arc<SiteHandle>`，必须 `as_ref()`）。
- 注意 `build_business_router` 仍注册全部 provider `callback_path`（集合），与站点无关；若 `cfg.sites` 非空，handler 层的站点校验在 `select_provider` 完成。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn per_site_spa_and_route_isolation() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    cfg.sites[0].spa = Some(SpaConfig { dir: common::make_spa_dir("a") });
    cfg.sites[1].spa = Some(SpaConfig { dir: common::make_spa_dir("b") });
    cfg.routes.push(route_with_site("/api/a", "app1"));
    cfg.routes.push(route_with_site("/api/b", "app2"));
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    // A 的 /api/b → 404（SPA fallback 前被 /api 前缀拦截）；B 的 /api/a → 404
    // A 的 /index.html 返回 a 目录内容；B 返回 b 目录内容
}
#[tokio::test]
async fn business_router_rejects_explicit_multisite() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));
    assert!(bff::server::business::build_business_router(state).is_err());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test test_multi_site_runtime`

- [ ] **Step 3: 实现**

把现有 `build_business_router` 主体抽为 `build_site_router`（首行 `let site = SiteCtx { handle: handle.as_ref(), view: state.site_view(&handle.name).expect("site view 必须存在") };`，handler 闭包内用 `Extension` 重新取 handle 构造 SiteCtx）；legacy 包装构建 legacy handle（`state.site_handles()` 中 `legacy == true` 的那个）。`session_info` 改为：
```rust
async fn session_info(State(state), Extension(handle), session: Session) -> Json<Value> {
    let view = state.site_view(&handle.name).expect("site view");
    let provider = view.current_provider(&session).await;
    Json(json!({ "logged_in": provider.is_some(), "provider": provider }))
}
```
`spa_dir` 不存在 → 保持现状 404 JSON。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`
Expected: 旧测试全绿 + 新测试绿。

- [ ] **Step 5: 提交**

```bash
git add src/server/business.rs tests/common/mod.rs tests/test_multi_site_runtime.rs
git commit -m "feat(multi-site): build per-site routers with site-scoped SPA and headers"
```

---

### Task 12: Host 校验中间件（§6.3）

**Files:**
- Create: `src/middleware/host_validation.rs`
- Modify: `src/middleware/mod.rs`
- Modify: `src/server/business.rs`（挂载：session layer 之外、`Extension` 之内）
- Test: `tests/test_host_validation.rs`（新）

**Interfaces:**
- `pub async fn host_validation_middleware(State(state), Extension(handle), req, next) -> Response`：
  - `/live`、`/ready` 无条件跳过；
  - `view.legacy && !cfg.server.enforce_host` → 跳过；
  - Host 缺失或归一化失败 → 421；
  - `view.allowed_hosts.contains(host)` 或（`view.public_base_url.is_none()` 且 `is_loopback_host(host)`）→ 放行；否则 421 JSON `{"error":"Misdirected Request"}`；
  - legacy `enforce_host=true` 时白名单 = `view.allowed_hosts`（已含 `trusted_hosts ∪ public host`），为空则仅 loopback。
- 顺序要求：该层必须在 session layer **之前**（`build_site_router` 中在 `.layer(session_layer)` 之后再加该 `.layer`，使其成为外层）。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn multisite_rejects_unknown_host_but_allows_public_and_dev_loopback() {
    // A: Host=evil.example.com → 421；Host=app1.example.com → 200（/live 与业务路径都验）
    // /live、/ready 用任意 Host → 200
}
#[tokio::test]
async fn loopback_rejected_when_public_base_url_configured() {
    // Host=localhost，多站点 app1（有 public_base_url）→ 421
}
#[tokio::test]
async fn legacy_without_enforce_host_is_unchanged() {
    // legacy 单站点、Host=evil，业务路径（SPA）→ 200，不返回 421
}
```
（测试用 `spawn_site` + 请求 `Host` 头覆写。）

- [ ] **Step 2: 运行确认失败**

- [ ] **Step 3: 实现**

`is_loopback_host` 复用/移动 `handlers.rs` 的实现到 `middleware/host_validation.rs`（`pub(crate)`），handlers 引用它。挂载位置按上面顺序要求。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/middleware/host_validation.rs src/middleware/mod.rs src/server/business.rs src/oidc/handlers.rs tests/test_host_validation.rs
git commit -m "feat(multi-site): enforce host allowlist with 421 for misdirected requests"
```

---

### Task 13: 跨站点 SSO 与令牌隔离集成测试（§7.3、§8、§14-2/3/4/13）

**Files:**
- Test: `tests/test_multi_site_sso.rs`（新）
- 可能的小修：`src/site.rs` / `src/oidc/handlers.rs`（测试暴露的缺陷）

**Interfaces:**
- Consumes: Task 11 的 `spawn_site`；`common::spawn_mock_oidc_provider`。
- 测试辅助（本文件内）：`async fn login_on(base: &str, provider: &str, cookie: Option<&str>) -> (String /*cookie value*/, ...)`：`GET /login?provider=` → 解析 authorize URL 的 `state`/`nonce` → 写 `idp.nonce` → `GET {callback_path}?code=mock&state=...`；返回 `set-cookie` 的 `BFF_SESSION_V2=<id>`（Domain cookie 在 127.0.0.1 上不会自动回传，后续请求显式带 `cookie:` 头模拟浏览器）。
- 测试用例：
  1. A 登录 → 共享会话建立；B 收到同一 cookie 的 `/api/session` 为 `logged_in=false`，受保护路由 401；
  2. B `GET /login?provider=<B provider>` → 302 → callback → 200/302；B 受保护路由 200 且上游收到 **B** 的 access token（`wiremock` + `header("authorization", "Bearer <B token>")`）；
  3. 越站：A 端口 `GET /login?provider=<B provider>` → 400（Review Focus #4）；响应体不含 `location`；
  4. 令牌隔离：B 的受保护路由绝不使用 A 的 token；
  5. 隔离性启动校验：`shared_across_sites=false` 的两站点共用 provider → `make_state` panic/`AppState::new` Err；`true` → 启动成功。

- [ ] **Step 1: 写失败测试**（上述 1–5）
- [ ] **Step 2: 运行确认失败**（如已实现则应通过；失败即修 `site.rs`/`handlers.rs`，记录 Ruling）
- [ ] **Step 3: 修复暴露的问题**（例如 callback 后未写站点 current_provider、`allowed_providers` 解析遗漏）
- [ ] **Step 4: 运行确认通过**

Run: `cargo test --test test_multi_site_sso -- --nocapture`

- [ ] **Step 5: 提交**

```bash
git add tests/test_multi_site_sso.rs src
git commit -m "test(multi-site): cross-site SSO, provider isolation and escape prevention"
```

---

### Task 14: 登出 scope 集成测试（§7.4、§14-7/14）

**Files:**
- Test: `tests/test_multi_site_logout.rs`（新）；复用 Task 13 的登录辅助（把辅助提升到 `tests/common/mod.rs`）

**Interfaces:**
- Consumes: Task 13 的 `common::login_on`/cookie 传递辅助。
- 用例：
  1. `logout_scope: global`：A/B 都登录 → A `/logout` → `logged_in=false`（A、B 均）、B 受保护路由 401；A 的登出响应为 IdP 302（mock IdP discovery 无 `end_session_endpoint` → `/`，两条路径都接受，断言不是 400/500）；
  2. `logout_scope: site`：A `/logout` → 302 `/`；A 受保护路由 401；B 仍 `logged_in=true` 且受保护路由 200；
  3. exchange 缓存清理：登录后向 `state.cache` 写入 `bff:token_exchange:{session_id}:cfgfp:subfp`（加密值随意），登出后 `state.cache.get` 为 None；
  4. 管理端 `DELETE /admin/api/sessions/:id` 在共享会话下等于全站剔除：调用后 A/B 均 401（文档化行为）。

- [ ] **Step 1: 写失败测试**（1–4）
- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 修复/实现**（site scope 的 token 清理、exchange 清理）
- [ ] **Step 4: 运行确认通过**

Run: `cargo test --test test_multi_site_logout -- --nocapture`

- [ ] **Step 5: 提交**

```bash
git add tests/common/mod.rs tests/test_multi_site_logout.rs src
git commit -m "test(multi-site): logout scopes and exchange cache revocation"
```

---

### Task 15: 多 listener 编排与异常退出语义（§6.1、§12）

**Files:**
- Create: `src/server/serve.rs`
- Modify: `src/server/mod.rs`、`src/main.rs`
- Test: `src/server/serve.rs` 内 `mod tests`

**Interfaces:**
- `pub type ServerFuture = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;`
- `pub async fn run_servers(servers: Vec<(String, ServerFuture)>, shutdown_tx: watch::Sender<bool>, shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<()>`：
  - 全部 `JoinSet::spawn`；
  - 第一个结束的任务：若 `Err(_)` 或（`Ok(())` 且 `*shutdown_rx.borrow() == false`）→ `shutdown_tx.send(true)`，`30s` 内 drain 其余，返回 `Err(anyhow!("服务 {name} 意外退出"))`；
  - 若 shutdown 已请求（信号）→ drain 其余，返回 `Ok(())`；
  - drain 超时仅 `warn` 后返回。
- `src/main.rs`：`AppState::new` → `verify_dependencies` → 后台 GC/watcher（不变）→ `let handles = state.site_handles()?`；逐站点 `TcpListener::bind((bind, port)).await?`（任一失败即启动失败）→ `axum::serve(listener, build_site_router(state.clone(), handle)?.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(watch_shutdown(rx.clone()))`；admin 同理；全部交给 `run_servers`。保留 30s drain 与 OTel shutdown。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn listener_failure_triggers_global_shutdown() {
    let (tx, rx) = watch::channel(false);
    let mut waiter = rx.clone();
    let servers = vec![
        ("bad".into(), Box::pin(async { Err(anyhow::anyhow!("boom")) }) as ServerFuture),
        ("wait".into(), Box::pin(async move {
            while !*waiter.borrow() { if waiter.changed().await.is_err() { break; } }
            Ok(())
        }) as ServerFuture),
    ];
    let err = run_servers(servers, tx, rx).await.unwrap_err();
    assert!(err.to_string().contains("bad"));
}
#[tokio::test]
async fn graceful_shutdown_returns_ok() { /* 两个 future 等 shutdown_rx 后 Ok；先 send(true) → run_servers Ok */ }
```

- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 实现**（`watch_shutdown` 从 main.rs 移入 `serve.rs` 或保持 `pub(crate)`）
- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/server/serve.rs src/server/mod.rs src/main.rs
git commit -m "feat(multi-site): multi-listener orchestration with fail-fast shutdown"
```

---

### Task 16: 管理面：`SessionInfo` additive + `GET /admin/api/sites` + 管理台站点列/模拟登录下拉

**Files:**
- Modify: `src/state.rs`（`SessionInfo` 增加 `#[serde(default)] pub sites: Vec<String>, pub providers: Vec<String>`）
- Modify: `src/admin/runtime_api.rs`（`list_sessions` 分页/有界并发推导、`list_sites`）
- Modify: `src/admin/mod.rs`（注册 `GET /admin/api/sites`）
- Modify: `admin-ui/src/types/index.ts`、`admin-ui/src/lib/api.ts`、`admin-ui/src/pages/Sessions.tsx`
- Test: `tests/test_admin_simulate_login.rs`（追加）、`tests/test_admin_endpoint.rs`（追加 `list_sites` 断言）

**Interfaces:**
- `list_sessions`：对 `state.sessions` 当前条目以 `futures::stream::iter(...).buffer_unordered(16)` 调 `state.session_store.load(id)`；record 缺失 → 跳过；`providers = record.data.keys().filter_map(|k| k.strip_prefix("oidc:").and_then(|r| r.strip_suffix(":tokens"))).collect()`；`sites` = 由 `cfg.effective_sites()` 中 `allowed_providers` 命中 providers 的站点名（去重、排序）。
- `GET /admin/api/sites` → `{"sites":[{"name","port","public_base_url","default_provider","providers":[...],"session_profile","logout_scope","legacy"}]}`。
- `admin-ui`：`SessionInfo` 增加 `provider/sites/providers/sub`；`Sessions.tsx` 增加“Provider”“站点”列；模拟登录 Dialog 增加站点下拉（数据来自 `listSites()`），origin = `site.public_base_url ?? http://${location.hostname}:${site.port}`；撤销会话确认文案改为“此操作将终止该用户在所有站点的会话”。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn list_sessions_derives_sites_and_providers() {
    // 在两站点共享会话上用 site A 登录（mock provider A），再手动补写 oidc:{pB}:tokens
    // GET /admin/api/sessions → sessions[0].providers 含 pA 与 pB，sites 含 app1、app2
}
#[tokio::test]
async fn admin_sites_endpoint_lists_ports() {
    // GET /admin/api/sites → sites[0].name=="app1" && sites[0].port==8081
}
```

- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 实现 Rust 侧；运行 `cargo test --all-features`**
- [ ] **Step 4: 实现 UI 侧并构建**

Run: `cd admin-ui && pnpm install --frozen-lockfile && pnpm build`
Expected: `tsc -b && vite build` 退出码 0。

- [ ] **Step 5: 提交**

```bash
git add src/state.rs src/admin tests admin-ui/src
git commit -m "feat(multi-site): admin sites API, session sites/providers and simulate-login selector"
```

---

### Task 17: 指标 `site` 标签与探针排除（§10、§14-8/9）

**Files:**
- Modify: `src/server/business.rs`
- Modify: `src/middleware/trace_context.rs`（`BffMakeSpan` 增加 `bff.site` 字段）
- Test: `tests/test_telemetry.rs`（追加）或 `src/server/business.rs` 单测

**Interfaces:**
- `metrics_middleware` 使用 `Extension<Arc<SiteHandle>>` 的 `handle.name` 作为 `site` 标签，`bff_http_requests_total{method,path,status,site}` 与 `bff_http_request_duration_seconds{method,path,site}`。
- `pub(crate) fn is_probe_path(path: &str) -> bool`：`/live`、`/ready`；为 true 时不记录业务指标。
- `metrics_path_label(state, site, path)` 用站点名做路由匹配。
- `BffMakeSpan::make_span` 从 `request.extensions().get::<axum::Extension<Arc<SiteHandle>>>()` 取站点名，span 增加字段 `"bff.site" = ...`（无 handle 时 `"-"`）；`Extension` 层必须在此层之外（Task 9 已保证最外层）才能读到。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn probe_paths_are_excluded_from_business_metrics() {
    assert!(is_probe_path("/live"));
    assert!(is_probe_path("/ready"));
    assert!(!is_probe_path("/api/x"));
}
#[tokio::test]
async fn business_metric_carries_site_label() {
    // spawn_site(app1) → GET /api/nonexistent → state.prometheus.render()
    // 断言包含 site="app1"（每个站点至少一条），且探针路径不出现在 bff_http_requests_total 的行内
}
```
探针排除的全局 recorder 断言可能与并行测试互扰：只断言 `cargo test -- --test-threads=1 --exact ...` 可复现；若仍不稳定，用 `is_probe_path` 单测作为主证据，集成断言只查 `site` 标签（在计划执行时记录 Ruling）。

- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 实现**
- [ ] **Step 4: 运行确认通过**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add src/server/business.rs tests
git commit -m "feat(multi-site): add site label to request metrics and exclude probes"
```

---

### Task 18: 部署资产：K8s 多端口、本地双子域 nginx、迁移演练（§11、§18-7/8）

**Files:**
- Modify: `deploy/k8s/deployment.yaml`、`deploy/k8s/service.yaml`、`deploy/k8s/ingress.yaml`
- Create: `deploy/multi-site/nginx.conf`、`deploy/multi-site/README.md`、`deploy/multi-site/config.example.yaml`（两站点 dev 配置示例：无 `server.public_base_url`，站点带 `server_names`/可选 `public_base_url`，`session.cookie_domain` + `allow_unmanaged_subdomains`）
- Create: `tests/test_multi_site_migration.rs`
- Test: YAML 语法 + 集成测试

**Interfaces:**
- K8s：Deployment `containerPort` 用两个站点名（`app1:8081`、`app2:8082`）+ `admin:8443`；探针打 `app1` 端口（`/live`、`/ready` 豁免 Host）；Service 两个按站点命名的端口；Ingress 两个 host 规则指向对应 service port。迁移推荐 `strategy: Recreate` 注释说明。
- `deploy/multi-site/nginx.conf`：`app1.localhost`/`app2.localhost` → `127.0.0.1:8081/8082`，`proxy_set_header Host $host;`、`proxy_set_header X-Forwarded-Proto $scheme;`；README 给出 `BFF_ENV=dev` + `config/sites.example.yaml` 启动步骤（站点 `public_base_url` 留空以走 dev loopback/Host 白名单语义，或使用 nginx Host）。
- `tests/test_multi_site_migration.rs`：构造两个独立 `AppState`（旧：`cookie_name = "BFF_SESSION"` + legacy 单站点；新：`cookie_name = "BFF_SESSION_V2"` + `cookie_domain: ".example.com"` + 两站点）；旧状态生成一个会话 cookie；用旧 cookie 请求新站点 → `logged_in=false` 且不 500（旧 cookie 被忽略）；在新状态完成登录 → 正常认证；重复执行（无状态依赖）稳定通过。

- [ ] **Step 1: 写迁移集成测试并运行失败/通过**

Run: `cargo test --test test_multi_site_migration -- --nocapture`

- [ ] **Step 2: 更新 K8s 清单**

Run: `kubectl kustomize deploy/k8s >/dev/null && echo OK`（无 kubectl 时：`python3 -c "import yaml,glob;[yaml.safe_load_all(open(f)) for f in glob.glob('deploy/k8s/*.yaml')];print('OK')"`）
Expected: `OK`

- [ ] **Step 3: 添加 nginx 示例与 README**（含 `curl` 手工验收步骤：`app1.localhost/live`、`app2.localhost/live`、Host 伪造 421、跨站 SSO）
- [ ] **Step 4: 运行全量测试**

Run: `cargo test --all-features`

- [ ] **Step 5: 提交**

```bash
git add deploy tests/test_multi_site_migration.rs
git commit -m "feat(multi-site): k8s manifests, local nginx example and migration drill"
```

---

### Task 19: 文档更新（§18-8/9）

**Files:**
- Modify: `docs/configuration.md`（新增 `sites` / `session_profiles` / `security_headers` 覆盖 / 校验规则 / 热生效对照更新）
- Modify: `docs/deployment.md`（多端口部署、两步发布与 Recreate、混版多次重认证、`proxy_next_upstream` 备注、回滚后 Cookie 清理）
- Modify: `docs/security-hardening.md`（共享域 Cookie 暴露面与 `allow_unmanaged_subdomains`、`__Host-` 互斥）
- Modify: `docs/runbook.md`（import `requires_restart` 处置、管理端删会话=全站踢出）
- Modify: `docs/architecture.md`（SiteHandle/SiteView/SiteCtx、多 listener、指标 `site`）
- Modify: `docs/README.md`（修正 `multi-site-design-v0.4.md` 缺失链接；把本计划加入索引）

**Interfaces:** 无代码接口；文档必须与 §5–§11 的最终行为一致，并在每处引用确切字段名（`sites[].logout_scope` 等）。

- [ ] **Step 1: 逐文件更新**，对照 spec 各节检查：
  - `docs/deployment.md`：加入 §8.4 IdP 注册清单模板（每站点独立 `client_id`、`redirect_uri = https://appN.example.com/auth/callback`、`post_logout_redirect_uri`）与 SameSite 交叉验证说明（Lax 兼容顶层导航回调；跨站 POST 回调需 `"None"` + `secure: true`）。
  - 验收 grep：`rg -n 'session_profiles|logout_scope|allow_unmanaged_subdomains|requires_restart' docs/{configuration,deployment,security-hardening,runbook,architecture}.md` 每个文件至少命中对应主题。
  - `docs/README.md` 不得链接不存在的文件：`rg -n 'multi-site-design' docs/README.md` 的结果逐行确认对应文件存在；若作者未提供 `docs/multi-site-design-v0.4.md`，删除该行并保留 `multi-site-design.md` 行。
- [ ] **Step 2: 校验命令**

Run: `for f in docs/configuration.md docs/deployment.md docs/security-hardening.md docs/runbook.md docs/architecture.md; do rg -q 'multi-site|站点' "$f" || echo "MISSING $f"; done`
Expected: 无输出。

- [ ] **Step 3: 提交**

```bash
git add docs
git commit -m "docs(multi-site): document config model, deployment and operations"
```

---

## 自检（写作时执行）

**Spec 覆盖：**

| Spec 节 | 覆盖 Task |
|---|---|
| §5.1–5.2 配置模型 / session_profiles | 1、2 |
| §5.3 路由归属 / 优先级 / 重复告警 | 3、8 |
| §5.4 全部启动校验（1–12） | 2、3、4 |
| §5.5 legacy 合成 | 1、11 |
| §5.6 热重载边界 / 结构指纹 / requires_restart | 5、7 |
| §6.1 多 listener / SiteHandle/SiteView/SiteCtx / 异常退出 | 6、11、15 |
| §6.2 处理器改造清单 | 9、11 |
| §6.3 Host 校验 421 | 12 |
| §7 会话与跨子域 SSO | 2、6、11、13 |
| §7.4 登出 scope | 9、14 |
| §8 OIDC 每站点 client / base_url / 刷新 | 9、13 |
| §9 SPA / 安全头预构建 | 6、11 |
| §10 管理面/指标/日志 | 16、17 |
| §11 部署 / 迁移 / 回滚 | 18、19 |
| §12 测试策略 | 各 Task + 13/14/18 集成 |
| §14 验收标准 1–15 | 1（1）、13（2/3/4/13）、12（5）、7（6/11）、14（7/14）、17（8/9）、各（10）、8（12）、18（15） |
| §18 前置产物 1–9 | 5（1）、2–4（2）、6/9/11（3）、4/5（4）、6/11（5）、16（6）、10/11/15/18（7）、18（8）、19（9） |

**Review Focus 落实：** #1→Task 3 测试 8；#2→Task 7 单测 + 集成；#3→Task 12；#4→Task 4 + Task 13；#5→Task 2 测试 5/6。

**类型一致性：** `SiteHandle/SiteView/SiteCtx` 只在 Task 6 定义一次；`select_provider(state, site, id)`、`current_tokens(site, session)`、`match_route(routes, site, ...)`、`build_site_router/build_business_router`、`apply_config/ConfigApplyError` 在后续任务中始终使用同一签名。

**P2 backlog（不进本计划）：** 管理台站点编辑；per-site CORS/限流/body limit；签名 Cookie；动态增删 listener；token exchange 零往返 SSO；SSO 安全区模式；token 分片存储；`global_local` scope；PSL 校验。

## Execution Handoff

执行方式（native inline vs subagent-driven）由人工在计划评审后选择；本计划与 spec 一并交给执行者。
