# `/api/me` 当前用户与权限视图 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** 为 BFF 增加同源只读端点 `GET /api/me`：SPA 不接触任何令牌即可拿到当前用户身份、会话状态与可配置的权限视图。

**Architecture:** 身份取自会话中已存的 ID Token（本地解码，零网络）；会话字段取自 `StoredTokens`（sub/provider/expires_at，绝不含令牌）。权限视图由站点级开关控制（全局 `me.permissions.enabled`，站点可覆盖），启用时以 Bearer access token 调下游授权权威 `/resolve`，结果按 `site+provider+sub` 缓存，失败/超时降级为「身份照常返回、permissions 不出现」。

**Tech Stack:** Rust 1.93 / axum 0.8 / tower-sessions 0.15 / serde + serde_yaml / humantime_serde / wiremock（测试）。

**Spec:** GitHub issue [#94](https://github.com/byteforce-cn/bff/issues/94)（外部源；以下为本计划逐字摘录的验收标准）：

- SPA 不接触任何令牌即可渲染「当前用户 + 权限呈现」；
- 未登录时返回 401 或 `logged_in: false`，不泄露用户信息；
- 权限视图可关闭（避免每个站点都强制依赖下游授权服务）。

评审决定（human partner 2026-10-11 确认方向「按你认为合理的处理方式」）：

- 路径用 `/api/me`（不是 `/api/userinfo`）：同源 cookie 端点不是 OIDC userinfo（Bearer/AS 语义），且响应含会话与权限视图，`userinfo` 名不副实；
- 未登录返回 **200 + `logged_in:false`**（与 `/api/session` 一致，SPA 一次调用兼做守卫与渲染）；
- 身份取本地 ID Token claims；不调上游 `/userinfo`（YAGNI，IdP 抖动不影响身份渲染）。

## Global Constraints

- 响应体**绝不出现** `access_token` / `refresh_token` / `id_token` 字段或值；身份只输出 `sub` / `preferred_username` / `name` / `email` 四个标准字段，不透传其它 claims。
- 权限视图**缺省关闭**；resolve 失败（非 2xx / 超时 / 结构非法 / 解密失败）必须降级：`200` + 身份字段照常，`permissions` 键**不出现**（不是 `null` / 空对象），且不得 5xx。
- 权限缓存键必须含 `site + provider + sub`（防跨站点/跨用户串数据）；`cache_ttl == 0` 时不缓存。
- `/api/session` 响应结构不变（multi-site 文档已承诺向后兼容）。
- 注释与文档使用中文，与仓库风格一致；错误文案含可定位的字段路径。
- 临时文件一律放 `/tmp`（AGENTS.md）；`target/` 为构建产物。

## Review Focus

- **ID Token 缺失或解码失败**（无 openid scope、上游返回不透明令牌、claims 非法）：`/api/me` 仍须 200 且 `sub` 回退到会话中的 `StoredTokens.sub`，不得 500。
- **resolve 返回 200 但结构非预期**（`roles` 为字符串、字段缺失、JSON 不可解析）：不得 panic / 不得输出半截 `permissions`，按失败降级并 warn。
- **resolve 慢或挂起**：`timeout` 必须真正生效（`tokio::time::timeout` 包住 send），否则 SPA 每次页面加载都被下游拖住。
- **权限缓存串数据**：不同站点、不同用户（同 provider）、登出再登录必须可能取到不同结果 —— 键含 `site+provider+sub`，TTL 到期自然失效。
- **claims 泄露边界**：IdP 在 ID Token 里塞了 `groups` / `internal_id` 等，`/api/me` 不得顺手透传（只允许四个白名单字段）。

---

### Task 1: `me` 配置段（模型 / 站点覆盖 / 校验）

**Files:**
- Modify: `src/config.rs`（`AppConfig`、`SiteConfig`、`ResolvedSite`、`effective_sites`、`validate_with_env`、`mod tests`）
- Modify: `src/config_fingerprint.rs:854`、`tests/common/mod.rs:305`、`tests/test_host_validation.rs:27`（`SiteConfig` 字面量补 `me: None`）
- Modify: `src/site.rs:314,626,633`、`src/oidc/handlers.rs:956`（`ResolvedSite` 字面量补 `me_permissions: None`）
- Modify: `docs/configuration.md`（新增 `### me` 段落 + TOC）

**Interfaces:**
- Consumes: 无。
- Produces（Task 2/3 依赖的确切签名与字段）：
  - `pub struct MeConfig { pub permissions: MePermissionsConfig }`（`AppConfig.me`，`#[serde(default)]`）
  - `pub struct MePermissionsConfig { pub enabled: bool, pub resolve_url: String, pub timeout: Duration, pub cache_ttl: Duration }`（默认 `enabled=false`、`resolve_url=""`、`timeout=3s`、`cache_ttl=30s`）
  - `pub struct SiteMeOverride { pub permissions_enabled: Option<bool> }`（`SiteConfig.me: Option<SiteMeOverride>`）
  - `pub struct ResolvedMePermissions { pub resolve_url: String, pub timeout: Duration, pub cache_ttl: Duration }`
  - `ResolvedSite.me_permissions: Option<ResolvedMePermissions>`（`None` = 该站点不返回权限视图）

- [x] **Step 1: 写失败测试**

追加到 `src/config.rs` 的 `#[cfg(test)] mod tests`（沿用同模块 `expect_ok` / `expect_err` 辅助）：

```rust
// ── me：当前用户视图配置（issue #94）──

#[test]
fn me_permissions_disabled_by_default() {
    let cfg: AppConfig = serde_yaml::from_str("{}").unwrap();
    assert!(!cfg.me.permissions.enabled);
    assert_eq!(cfg.me.permissions.timeout, std::time::Duration::from_secs(3));
    assert_eq!(cfg.me.permissions.cache_ttl, std::time::Duration::from_secs(30));
    let sites = cfg.effective_sites();
    assert!(sites[0].me_permissions.is_none(), "缺省关闭");
}

#[test]
fn me_permissions_enabled_resolves_for_legacy_site() {
    let cfg: AppConfig = serde_yaml::from_str(
        r#"
me:
  permissions:
    enabled: true
    resolve_url: "https://iam.internal/api/resolve"
"#,
    )
    .unwrap();
    cfg.validate_with_env(false).unwrap();
    let sites = cfg.effective_sites();
    let p = sites[0].me_permissions.as_ref().expect("legacy 站点应启用");
    assert_eq!(p.resolve_url, "https://iam.internal/api/resolve");
}

#[test]
fn me_permissions_site_override_wins_over_global() {
    let cfg: AppConfig = serde_yaml::from_str(
        r#"
me:
  permissions:
    enabled: true
    resolve_url: "https://iam.internal/api/resolve"
sites:
  - name: app1
    port: 8081
  - name: app2
    port: 8082
    me:
      permissions_enabled: false
"#,
    )
    .unwrap();
    cfg.validate_with_env(false).unwrap();
    let sites = cfg.effective_sites();
    assert!(sites.iter().find(|s| s.name == "app1").unwrap().me_permissions.is_some());
    assert!(sites.iter().find(|s| s.name == "app2").unwrap().me_permissions.is_none());
}

#[test]
fn me_permissions_enabled_without_resolve_url_rejected() {
    expect_err(
        r#"
me:
  permissions:
    enabled: true
"#,
        "me.permissions.resolve_url",
    );
}

#[test]
fn me_permissions_rejects_non_http_resolve_url() {
    expect_err(
        r#"
me:
  permissions:
    enabled: true
    resolve_url: "iam.internal/resolve"
"#,
        "me.permissions.resolve_url",
    );
}

/// 站点单独开启（全局关闭）同样要求 resolve_url，否则站点启用后必然失败。
#[test]
fn me_permissions_site_enable_requires_resolve_url() {
    expect_err(
        r#"
sites:
  - name: app1
    port: 8081
    me:
      permissions_enabled: true
"#,
        "me.permissions.resolve_url",
    );
}
```

- [x] **Step 2: 跑测试，确认 RED**

Run: `cargo test --lib config::tests::me_permissions -- --nocapture`
Expected: 编译失败 `no field 'me' on type AppConfig` / `cannot find ...`（功能尚未实现）。

- [x] **Step 3: 实现配置模型与解析**

在 `src/config.rs`：

1. `AppConfig` 增加字段（放在 `pub sites` 之前）：

```rust
    /// 当前用户视图（`GET /api/me`）：默认只返回身份与会话，权限视图缺省关闭
    #[serde(default)]
    pub me: MeConfig,
```

2. 在 `SiteConfig` 增加：

```rust
    /// 站点级 `/api/me` 覆盖（未指定字段继承全局 `me`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<SiteMeOverride>,
```

3. 新增类型与默认值（放在 `SiteConfig` 附近）：

```rust
/// `/api/me` 端点配置（issue #94）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeConfig {
    /// 权限视图（下游授权权威解析；缺省关闭）
    #[serde(default, skip_serializing_if = "MePermissionsConfig::is_default")]
    pub permissions: MePermissionsConfig,
}

/// 权限视图配置：启用时以 Bearer access token 调用 `resolve_url`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MePermissionsConfig {
    /// 是否返回权限视图（站点可覆盖；缺省关闭）
    #[serde(default)]
    pub enabled: bool,
    /// 下游授权权威 resolve 端点（`enabled` 时必填，http(s) 绝对 URL）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolve_url: String,
    /// 单次 resolve 调用超时（默认 3s）
    #[serde(default = "default_me_resolve_timeout", with = "humantime_serde")]
    pub timeout: Duration,
    /// resolve 结果缓存 TTL（默认 30s；0 = 不缓存）
    #[serde(default = "default_me_permissions_cache_ttl", with = "humantime_serde")]
    pub cache_ttl: Duration,
}

impl Default for MePermissionsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            resolve_url: String::new(),
            timeout: default_me_resolve_timeout(),
            cache_ttl: default_me_permissions_cache_ttl(),
        }
    }
}

impl MePermissionsConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn default_me_resolve_timeout() -> Duration {
    Duration::from_secs(3)
}

fn default_me_permissions_cache_ttl() -> Duration {
    Duration::from_secs(30)
}

/// 站点级 `/api/me` 覆盖（Partial：`None` = 继承全局）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteMeOverride {
    /// 是否返回权限视图；`None` 继承全局 `me.permissions.enabled`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions_enabled: Option<bool>,
}

/// 站点已解析的权限视图配置（`None` = `/api/me` 不返回 permissions）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMePermissions {
    pub resolve_url: String,
    pub timeout: Duration,
    pub cache_ttl: Duration,
}
```

4. `ResolvedSite` 增加字段：

```rust
    /// 已解析的权限视图配置（`None` = 不返回 permissions）
    pub me_permissions: Option<ResolvedMePermissions>,
```

5. `effective_sites()`：legacy 分支与显式分支都填：

```rust
                // legacy 分支
                me_permissions: resolve_me_permissions(&self.me, None),
```
```rust
                    // 显式站点分支
                    me_permissions: resolve_me_permissions(&self.me, site.me.as_ref()),
```

并加辅助函数：

```rust
/// 合并全局 `me.permissions` 与站点覆盖：站点 `Some` 优先，否则继承全局开关。
fn resolve_me_permissions(
    global: &MeConfig,
    site: Option<&SiteMeOverride>,
) -> Option<ResolvedMePermissions> {
    let enabled = site
        .and_then(|s| s.permissions_enabled)
        .unwrap_or(global.permissions.enabled);
    enabled.then(|| ResolvedMePermissions {
        resolve_url: global.permissions.resolve_url.clone(),
        timeout: global.permissions.timeout,
        cache_ttl: global.permissions.cache_ttl,
    })
}
```

6. `validate_with_env` 在 `self.validate_sites(is_prod, &sites)?;`（`src/config.rs:2142`）之后加 `self.validate_me()?;`，并新增：

```rust
/// `me.permissions` 校验：任一站点（含 legacy 全局）启用权限视图时，
/// `resolve_url` 必须是非空 http(s) 绝对 URL。
fn validate_me(&self) -> anyhow::Result<()> {
    let p = &self.me.permissions;
    let any_enabled = p.enabled
        || self.sites.iter().any(|s| {
            s.me
                .as_ref()
                .and_then(|m| m.permissions_enabled)
                .unwrap_or(false)
        });
    if !any_enabled {
        return Ok(());
    }
    anyhow::ensure!(
        !p.resolve_url.is_empty(),
        "me.permissions.resolve_url 不能为空（权限视图已启用）"
    );
    anyhow::ensure!(
        p.resolve_url.starts_with("http://") || p.resolve_url.starts_with("https://"),
        "me.permissions.resolve_url 必须是 http(s) 绝对 URL: {}",
        p.resolve_url
    );
    anyhow::ensure!(
        p.timeout > Duration::ZERO,
        "me.permissions.timeout 必须大于 0（权限视图已启用）"
    );
    Ok(())
}
```

7. 修字面量编译错误：`SiteConfig { ... }` 补 `me: None`，`ResolvedSite { ... }` 补 `me_permissions: None`（文件见 Files）。

- [x] **Step 4: 跑测试，确认 GREEN**

Run: `cargo test --lib config::tests::me_permissions -- --nocapture`
Expected: `test result: ok. 6 passed`。

- [x] **Step 5: 文档（同一个提交）**

`docs/configuration.md`：TOC 在 `### sites` 后加 `### me — 当前用户视图与权限`，正文加：

```markdown
### me — 当前用户视图与权限

`GET /api/me` 返回当前用户身份（`sub` / `preferred_username` / `name` / `email`，取自 ID Token）、
会话（`logged_in` / `provider` / `expires_at`，**不含任何令牌**）与可选的权限视图。未登录返回
`200 {"logged_in": false, "provider": null}`，不泄露任何用户信息。

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `permissions.enabled` | `false` | 是否返回权限视图（**缺省关闭**，避免所有站点强制依赖下游授权服务） |
| `permissions.resolve_url` | 必填（启用时） | 下游授权权威 resolve 端点（http/https 绝对 URL） |
| `permissions.timeout` | `3s` | 单次 resolve 调用超时；失败/超时降级为不返回 `permissions`（身份照常 200） |
| `permissions.cache_ttl` | `30s` | resolve 结果缓存 TTL（`0` = 不缓存） |

站点级覆盖：`sites[].me.permissions_enabled`（`true`/`false`），未指定继承全局开关。
resolve 调用契约为 `POST` + `Authorization: Bearer <access token>` +
body `{"sub": "...", "provider": "..."}`，响应 `2xx {"roles": [...], "permissions": [...]}`。
```

- [x] **Step 6: 提交**

```bash
git add src/config.rs src/config_fingerprint.rs src/site.rs src/oidc/handlers.rs \
  tests/common/mod.rs tests/test_host_validation.rs docs/configuration.md
git commit -m "feat(me): add me.permissions config with per-site override and validation"
```

---

### Task 2: `GET /api/me` 身份视图（路由 + handler）

**Files:**
- Create: `src/server/me.rs`
- Modify: `src/server/mod.rs`（`pub mod me;`）
- Modify: `src/server/business.rs`（路由注册 + metrics `FIXED` 标签）
- Modify: `src/site.rs`（`SiteView.me_permissions` 字段 + `from_resolved` 赋值）
- Create: `tests/test_me_endpoint.rs`
- Modify: `docs/architecture.md`（路由清单）、`docs/multi-site-design.md:390`（保留路径）

**Interfaces:**
- Consumes（Task 1）：`ResolvedMePermissions`、`SiteView.me_permissions: Option<ResolvedMePermissions>`。
- Produces（Task 3 依赖）：
  - `pub async fn me_handler(State<AppState>, Extension<Arc<SiteHandle>>, Session) -> Json<serde_json::Value>`
  - `pub(crate) fn decode_id_token_claims(id_token: &str) -> Option<serde_json::Value>`
  - `pub(crate) fn identity_fields(sub: &str, claims: Option<&serde_json::Value>) -> serde_json::Value`

- [x] **Step 1: 写失败测试（单元 + 集成）**

`src/server/me.rs` 底部 `#[cfg(test)] mod tests`（连同 handler 骨架一起创建，见 Step 3）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_id_token_claims_rejects_malformed() {
        assert!(decode_id_token_claims("not-a-jwt").is_none());
        assert!(decode_id_token_claims("a.!!!.c").is_none());
    }

    #[test]
    fn identity_fields_keeps_only_standard_string_claims() {
        let claims = serde_json::json!({
            "sub": "ignored",
            "preferred_username": "alice",
            "name": "Alice",
            "email": "alice@example.test",
            "groups": ["internal"],
            "nonce": "n",
            "iat": 1
        });
        let v = identity_fields("user-1", Some(&claims));
        assert_eq!(v["sub"], "user-1", "sub 以会话为准");
        assert_eq!(v["preferred_username"], "alice");
        assert_eq!(v["name"], "Alice");
        assert_eq!(v["email"], "alice@example.test");
        assert!(v.get("groups").is_none(), "非白名单 claims 不得透传");
        assert!(v.get("nonce").is_none());
        assert!(v.get("iat").is_none());
    }

    #[test]
    fn identity_fields_omits_missing_or_non_string_claims() {
        let claims = serde_json::json!({"name": 42, "email": {"a": 1}});
        let v = identity_fields("user-1", Some(&claims));
        assert_eq!(v.as_object().unwrap().len(), 1);
        assert_eq!(v["sub"], "user-1");
    }
}
```

`tests/test_me_endpoint.rs`：

```rust
//! `GET /api/me` 身份与会话视图（issue #94）：不泄露令牌、未登录不泄露用户信息。
mod common;

use bff::state::AppState;

/// 由测试直接构造的未签名 ID Token（`alg: none`；`/api/me` 只解码本地 claim，不验签）。
fn unsigned_id_token(claims: serde_json::Value) -> String {
    use base64::Engine;
    let b64 = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap())
    };
    format!(
        "{}.{}.",
        b64(serde_json::json!({"alg": "none", "typ": "JWT"})),
        b64(claims)
    )
}

/// 写入一个带身份 claims 的登录会话（`sub` 取自 claims，缺省 `user-1`），返回 Cookie 头值。
async fn login_with_claims(state: &AppState, claims: serde_json::Value) -> String {
    let sub = claims["sub"].as_str().unwrap_or("user-1").to_string();
    login_with_sub(state, &sub, claims).await
}

/// 指定 `sub` 的登录会话（权限缓存隔离用例需要两个不同用户）。
async fn login_with_sub(state: &AppState, sub: &str, claims: serde_json::Value) -> String {
    let id_token = unsigned_id_token(claims);
    let tokens = bff::oidc::StoredTokens::new(
        "mock",
        sub,
        "test-access-token",
        None,
        Some(&id_token),
        3600,
    )
    .expect("构造 StoredTokens 失败");
    common::create_session_with_tokens(state, &tokens).await
}

fn legacy_state_with_provider() -> AppState {
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    common::make_state(cfg)
}

#[tokio::test]
async fn me_reports_logged_out_without_session() {
    let state = legacy_state_with_provider();
    let bff = common::spawn_business(state).await;
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // 精确相等：未登录响应不得夹带任何身份字段
    assert_eq!(body, serde_json::json!({"logged_in": false, "provider": null}));
}

#[tokio::test]
async fn me_returns_identity_and_session_when_logged_in() {
    let state = legacy_state_with_provider();
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(
        &state,
        serde_json::json!({
            "sub": "user-1",
            "preferred_username": "alice",
            "name": "Alice",
            "email": "alice@example.test"
        }),
    )
    .await;
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["provider"], "mock");
    assert_eq!(body["sub"], "user-1");
    assert_eq!(body["preferred_username"], "alice");
    assert_eq!(body["name"], "Alice");
    assert_eq!(body["email"], "alice@example.test");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(body["expires_at"].as_i64().unwrap() > now);
    let raw = serde_json::to_string(&body).unwrap();
    for needle in ["access_token", "refresh_token", "id_token", "test-access-token"] {
        assert!(!raw.contains(needle), "响应不得含 {needle}: {raw}");
    }
}

#[tokio::test]
async fn me_falls_back_to_session_sub_without_id_token() {
    let state = legacy_state_with_provider();
    let bff = common::spawn_business(state.clone()).await;
    // login_cookie 写入的 StoredTokens 无 id_token（无 openid scope 场景）
    let cookie = common::login_cookie(&state).await;
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["sub"], "test-user");
    assert!(body.get("name").is_none());
    assert!(body.get("email").is_none());
}
```

> `common::create_session_with_tokens` 返回 legacy Cookie 头值 `BFF_SESSION=<id>`（见 `tests/common/mod.rs:384`）。

- [x] **Step 2: 跑测试，确认 RED**

Run: `cargo test --test test_me_endpoint -- --nocapture`
Expected: 请求 `/api/me` 返回 404（未注册路由），断言 `assert_eq!(resp.status(), 200)` 失败。

- [x] **Step 3: 实现最小 handler + 路由**

`src/server/mod.rs` 加 `pub mod me;`。

`src/server/me.rs`：

```rust
//! `GET /api/me` — 当前用户与会话视图（同源只读，绝不含任何令牌）。
//!
//! 身份取会话中已存的 ID Token claims（本地解码，零网络）；权限视图
//! （`me.permissions`，缺省关闭）由 Task 3 追加。
use crate::site::SiteHandle;
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::response::Json;
use std::sync::Arc;
use tower_sessions::Session;

/// 本地解码 ID Token 载荷（不验签：令牌来自本端加密会话，非外部输入）。
/// 非法结构返回 `None`，由调用方回退到会话中的 `sub`。
pub(crate) fn decode_id_token_claims(id_token: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 白名单身份字段：`sub`（以会话为准）+ ID Token 中的三个标准字符串 claim。
/// 其它 claims（groups / nonce / iat …）一律不透传。
pub(crate) fn identity_fields(sub: &str, claims: Option<&serde_json::Value>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("sub".into(), serde_json::Value::String(sub.to_string()));
    for key in ["preferred_username", "name", "email"] {
        if let Some(v) = claims.and_then(|c| c.get(key)).and_then(|v| v.as_str()) {
            out.insert(key.into(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::Value::Object(out)
}

/// GET /api/me — 站点感知：登录态与身份按站点当前 provider 解析（与 `/api/session` 同源）。
pub async fn me_handler(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
) -> Json<serde_json::Value> {
    let view = state.site_view(&handle.name).expect("site view");
    let Some(provider) = view.current_provider(&session).await else {
        return Json(logged_out());
    };
    let Some(tokens) = view.current_tokens(&session).await else {
        // current_provider 已确认令牌存在；读取失败按未登录处理（防御，不 500）
        return Json(logged_out());
    };
    let id_token = tokens.id_token().ok().flatten();
    let claims = id_token.as_deref().and_then(decode_id_token_claims);
    let mut body = identity_fields(&tokens.sub, claims.as_ref());
    if let Some(obj) = body.as_object_mut() {
        obj.insert("logged_in".into(), serde_json::Value::Bool(true));
        obj.insert("provider".into(), serde_json::Value::String(provider));
        obj.insert("expires_at".into(), serde_json::Value::from(tokens.expires_at));
    }
    Json(body)
}

fn logged_out() -> serde_json::Value {
    serde_json::json!({"logged_in": false, "provider": null})
}
```

`src/site.rs`：`SiteView` 加 `pub me_permissions: Option<crate::config::ResolvedMePermissions>,`，
`from_resolved` 加 `me_permissions: site.me_permissions.clone(),`。

`src/server/business.rs`：`.route("/api/session", get(session_info))` 之后加
`.route("/api/me", get(crate::server::me::me_handler))`；`metrics_path_label` 的
`FIXED` 数组补 `"/api/me"`。

- [x] **Step 4: 跑测试，确认 GREEN**

Run: `cargo test --test test_me_endpoint -- --nocapture && cargo test --lib server::me -- --nocapture`
Expected: `test result: ok`（集成 3 passed + 单元 3 passed）。

- [x] **Step 5: 文档（同一个提交）**

- `docs/architecture.md` 路由清单在「探针」前加一行：
  `- 当前用户视图：\`/api/me\`（身份 + 会话；\`me.permissions\` 启用时含下游权限视图，缺省关闭）；`
- `docs/multi-site-design.md:390` 保留路径列表加 `/api/me`（与 `/api/session` 并列）。

- [x] **Step 6: 提交**

```bash
git add src/server/me.rs src/server/mod.rs src/server/business.rs src/site.rs \
  tests/test_me_endpoint.rs docs/architecture.md docs/multi-site-design.md
git commit -m "feat(me): add GET /api/me identity and session view"
```

---

### Task 3: 权限视图（resolve + 缓存 + 降级）

**Files:**
- Modify: `src/server/me.rs`（`permissions_fields` / `resolve_permissions` / handler 接线）
- Modify: `tests/test_me_endpoint.rs`（wiremock 用例）
- Modify: `CHANGELOG.md`（Unreleased / Added）

**Interfaces:**
- Consumes（Task 1/2）：`ResolvedMePermissions`、`SiteView.me_permissions`、`me_handler` 内的身份视图。
- Produces：`pub(crate) fn permissions_fields(resolve_body: &serde_json::Value) -> Option<serde_json::Value>`。

- [ ] **Step 1: 写失败测试**

`src/server/me.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn permissions_fields_filters_and_defaults() {
        let v = permissions_fields(&serde_json::json!({
            "roles": ["admin", 1],
            "permissions": ["user:read"]
        }))
        .expect("roles/permissions 数组应被接受");
        assert_eq!(v["roles"], serde_json::json!(["admin"]));
        assert_eq!(v["permissions"], serde_json::json!(["user:read"]));

        // 只有一个键：另一个补空数组
        let v = permissions_fields(&serde_json::json!({"roles": []})).unwrap();
        assert_eq!(v["permissions"], serde_json::json!([]));

        // 结构非法：按键存在但非数组 → 整体拒绝（不输出半截字段）
        assert!(permissions_fields(&serde_json::json!({"roles": "admin"})).is_none());
        assert!(permissions_fields(&serde_json::json!({})).is_none());
        assert!(permissions_fields(&serde_json::json!({"other": []})).is_none());
    }
```

`tests/test_me_endpoint.rs` 追加（wiremock）：

```rust
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 启用权限视图、指向 mock resolve 服务的配置。
fn state_with_permissions(resolve_url: &str) -> AppState {
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = resolve_url.to_string();
    common::make_state(cfg)
}

async fn me_body(bff: &str, cookie: &str) -> serde_json::Value {
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn me_returns_permissions_from_authority() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/resolve"))
        .and(header("authorization", "Bearer test-access-token"))
        .and(body_json(serde_json::json!({"sub": "user-1", "provider": "mock"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            "permissions": ["user:read", "user:write"]
        })))
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["sub"], "user-1");
    assert_eq!(body["permissions"]["roles"], serde_json::json!(["admin"]));
    assert_eq!(
        body["permissions"]["permissions"],
        serde_json::json!(["user:read", "user:write"])
    );
    resolve.verify().await;
}

#[tokio::test]
async fn me_omits_permissions_by_default_and_never_calls_resolve() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"roles": ["admin"]})))
        .expect(0)
        .mount(&resolve)
        .await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.resolve_url = format!("{}/resolve", resolve.uri()); // enabled 保持 false
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert!(body.get("permissions").is_none(), "缺省关闭: {body}");
    resolve.verify().await;
}

#[tokio::test]
async fn me_degrades_to_identity_when_resolve_fails() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1", "name": "Alice"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["name"], "Alice", "身份视图不得被 resolve 失败拖垮");
    assert!(body.get("permissions").is_none(), "失败时 permissions 不出现: {body}");
    resolve.verify().await;
}

#[tokio::test]
async fn me_degrades_to_identity_when_resolve_times_out() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(900))
                .set_body_json(serde_json::json!({"roles": ["admin"]})),
        )
        .expect(1)
        .mount(&resolve)
        .await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = format!("{}/resolve", resolve.uri());
    cfg.me.permissions.timeout = Duration::from_millis(200);
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert!(body.get("permissions").is_none());
    resolve.verify().await;
}

#[tokio::test]
async fn me_caches_permissions_between_calls() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            "permissions": ["user:read"]
        })))
        .expect(1) // 第二次调用必须命中缓存
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    assert_eq!(me_body(&bff, &cookie).await["permissions"]["roles"][0], "admin");
    assert_eq!(me_body(&bff, &cookie).await["permissions"]["roles"][0], "admin");
    resolve.verify().await;
}

/// 权限缓存按 `sub` 隔离：两个用户不得互相命中（键必须含 sub）。
#[tokio::test]
async fn me_permissions_cache_is_per_user() {
    let resolve = MockServer::start().await;
    for (sub, role) in [("user-1", "role-1"), ("user-2", "role-2")] {
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({"sub": sub, "provider": "mock"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "roles": [role],
                "permissions": []
            })))
            .expect(1)
            .mount(&resolve)
            .await;
    }
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie_1 = login_with_sub(&state, "user-1", serde_json::json!({"sub": "user-1"})).await;
    let cookie_2 = login_with_sub(&state, "user-2", serde_json::json!({"sub": "user-2"})).await;

    assert_eq!(me_body(&bff, &cookie_1).await["permissions"]["roles"][0], "role-1");
    assert_eq!(me_body(&bff, &cookie_2).await["permissions"]["roles"][0], "role-2");
    resolve.verify().await;
}

/// Review Focus 2：resolve 返回 2xx 但结构非法（roles 非数组）→ 身份照常、permissions 不出现。
#[tokio::test]
async fn me_degrades_when_resolve_shape_is_invalid() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"roles": "admin"})))
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert!(body.get("permissions").is_none(), "结构非法时整体拒绝: {body}");
    resolve.verify().await;
}

/// 站点策略：全局启用 + 站点显式 `false` → 该站点不返回 permissions，也不触发 resolve。
#[tokio::test]
async fn me_site_policy_can_disable_permissions() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            "permissions": []
        })))
        .expect(1) // 仅 app1 触发
        .mount(&resolve)
        .await;

    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = format!("{}/resolve", resolve.uri());
    cfg.sites[1].me = Some(bff::config::SiteMeOverride {
        permissions_enabled: Some(false),
    });
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    let client = common::test_client();
    // A 先断言：B 的静默续登会轮换共享会话 id（cycle_id），旧 cookie 随即失效
    let cookie_a = common::login_on(&client, &idp_a, &a, "pA", None).await;
    let body_a = me_body(&a, &cookie_a).await;
    assert_eq!(body_a["permissions"]["roles"][0], "admin");
    let cookie_b = common::login_on(&client, &idp_b, &b, "pB", Some(&cookie_a)).await;
    let body_b = me_body(&b, &cookie_b).await;
    assert_eq!(body_b["logged_in"], true);
    assert!(body_b.get("permissions").is_none(), "站点显式关闭: {body_b}");
    resolve.verify().await;
}
```

- [ ] **Step 2: 跑测试，确认 RED**

Run: `cargo test --test test_me_endpoint -- --nocapture`
Expected: `permissions` 相关用例失败（`body.get("permissions").is_none()` 或 `roles[0]` 索引 panic），且 resolve 请求计数为 0。

- [ ] **Step 3: 实现 resolve + 缓存 + 降级**

`src/server/me.rs` 追加（handler 在身份字段写完后接线）：

```rust
/// 权限视图解析结果校验：`roles` / `permissions` 至少出现一个且必须是字符串数组；
/// 结构非法返回 `None`（整体拒绝，不输出半截字段）。
pub(crate) fn permissions_fields(resolve_body: &serde_json::Value) -> Option<serde_json::Value> {
    fn strings(v: &serde_json::Value) -> Option<Vec<String>> {
        let arr = v.as_array()?;
        Some(
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        )
    }
    let has_any = resolve_body.get("roles").is_some() || resolve_body.get("permissions").is_some();
    if !has_any {
        return None;
    }
    // 键缺失 = 该维度为空；键存在但非数组 = 结构非法（整体拒绝）
    let roles = match resolve_body.get("roles") {
        Some(v) => strings(v)?,
        None => Vec::new(),
    };
    let permissions = match resolve_body.get("permissions") {
        Some(v) => strings(v)?,
        None => Vec::new(),
    };
    Some(serde_json::json!({"roles": roles, "permissions": permissions}))
}

/// 调用下游授权权威解析权限；任何失败/超时/结构非法都返回 `None`（降级为身份视图）。
/// 成功结果按 `site + provider + sub` 缓存，TTL 见 `me.permissions.cache_ttl`。
async fn resolve_permissions(
    state: &AppState,
    site: &SiteView,
    tokens: &StoredTokens,
    cfg: &ResolvedMePermissions,
) -> Option<serde_json::Value> {
    let key = format!(
        "bff:me:permissions:{}:{}:{}",
        site.name, tokens.provider, tokens.sub
    );
    if let Some(bytes) = state.cache.get(&key).await {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            return Some(v);
        }
    }
    let access_token = match tokens.access_token() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "权限视图：access token 解密失败，降级");
            return None;
        }
    };
    let request = state
        .http
        .post(&cfg.resolve_url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {access_token}"))
        .json(&serde_json::json!({"sub": tokens.sub, "provider": tokens.provider}));
    let resp = match tokio::time::timeout(cfg.timeout, request.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "权限视图：resolve 调用失败，降级");
            return None;
        }
        Err(_) => {
            tracing::warn!(timeout_ms = cfg.timeout.as_millis(), "权限视图：resolve 超时，降级");
            return None;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(%status, "权限视图：resolve 返回非 2xx，降级");
        return None;
    }
    let body = match resp.json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "权限视图：resolve 响应非法 JSON，降级");
            return None;
        }
    };
    let Some(permissions) = permissions_fields(&body) else {
        tracing::warn!("权限视图：resolve 响应缺少 roles/permissions 数组，降级");
        return None;
    };
    if let Ok(bytes) = serde_json::to_vec(&permissions) {
        state.cache.set(&key, bytes, cfg.cache_ttl).await;
    }
    Some(permissions)
}
```

handler 中身份字段写完后：

```rust
    if let Some(cfg) = view.me_permissions.as_ref() {
        if let Some(permissions) = resolve_permissions(&state, &view, &tokens, cfg).await {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("permissions".into(), permissions);
            }
        }
    }
```

补充 import：`use crate::config::ResolvedMePermissions; use crate::oidc::tokens::StoredTokens; use crate::site::SiteView;`。

- [ ] **Step 4: 跑测试，确认 GREEN**

Run: `cargo test --test test_me_endpoint -- --nocapture && cargo test --lib server::me -- --nocapture`
Expected: `test result: ok`（集成 11 passed + 单元 4 passed）。

- [ ] **Step 5: 文档（同一个提交）**

`CHANGELOG.md` 的 `## [Unreleased]` 下新增：

```markdown
### Added

- `GET /api/me`：同源只读的「当前用户与会话」视图（身份取自 ID Token，**不含任何令牌**）；未登录返回 `200 {"logged_in": false}`；可选的权限视图（`me.permissions`，缺省关闭，站点可覆盖）由下游授权权威 `/resolve` 解析并按 `site+provider+sub` 缓存，失败/超时降级为不返回该字段
```

- [ ] **Step 6: 提交**

```bash
git add src/server/me.rs tests/test_me_endpoint.rs CHANGELOG.md
git commit -m "feat(me): resolve optional permission view with cache and graceful degradation"
```

---

## 收尾验证（三个任务全部完成后）

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all-features`（全量，含既有回归）
- [ ] 整分支 review（fresh reviewer）：对照 issue #94 验收标准与 Global Constraints，确认无令牌泄露、无 5xx 降级路径。
