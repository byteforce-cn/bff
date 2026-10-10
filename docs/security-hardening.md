# 安全加固说明

本文档汇总 BFF 内置的安全能力、生效方式与**验证手段**，供安全评估、试点与上线自查使用。

> 适用范围：v0.1.x（beta）。BFF 已完成系统性的安全加固与多轮真实环境验证，
> 但尚未进行外部渗透测试；生产使用前请完成 [deployment.md](deployment.md) §6 的逐项自查。

## 1. 认证与会话

| 能力 | 说明 |
| --- | --- |
| OIDC 授权码 + PKCE | 强制 `S256`；`state` / `nonce` 校验；ID Token 真实 JWKS 验签（RS256） |
| 会话固定防护 | 登录成功后轮换会话 ID 并重新登记 |
| 会话 Cookie | 默认 `HttpOnly` + `Secure` + `SameSite=Lax`（跨站点 IdP 场景实测必需；同站部署可改回 `Strict`） |
| 令牌保护 | 会话内令牌 AES-256-GCM 加密存储；刷新为 SWR + 分布式锁（防惊群） |
| 登出 | 优先使用 discovery 的 `end_session_endpoint`；失败回退本地清会话并告警 |
| 会话治理 | 服务端会话索引后台 GC；`last_seen` 节流更新；登出 / 撤销时同步清理关联缓存 |

**验证**：`tests/test_oidc_signature.rs`（进程内 RS256 签名 IdP + JWKS，覆盖伪造密钥 / `alg:none` / nonce 不一致三类攻击拒绝）；Keycloak 26 真实 IdP 契约 E2E（见 §8）。

### 多站点共享域 Cookie（暴露面）

多站点 SSO 使用 `session.cookie_domain`（Domain cookie，如 `.example.com`）在站点子域间共享会话：

- **暴露面**：该 Cookie 会发送给 `.example.com` 下**所有**子域（含未被 BFF 托管的服务）；域内任一子域被攻破即可窃取会话（会话记录含全部站点 token）；
- **信任边界显式确认**：`session.allow_unmanaged_subdomains: true`（或对应 `session_profiles.<name>.allow_unmanaged_subdomains`）声明“已知该域内未托管子域也会收到会话 Cookie”。prod 下 `cookie_domain` 非空却未确认 → **拒绝启动**，dev 仅告警；
- **正向 domain-match 校验**：`cookie_domain` 必须是引用该 profile 的每个站点主机的父域，否则启动失败；
- **`__Host-` 前缀互斥**：`__Host-` 前缀的 Cookie 要求无 `Domain` 且 `Path=/`，与 Domain cookie **无法并用**；共享会话只能依赖 `Secure` + `HttpOnly` + `SameSite` + 会话 ID 轮换（登录后 `cycle_id()`）；
- 会话记录承载全部站点 token，整条 last-write-wins（跳站并发写为低概率，见设计 §7.5）；单 record 规模随站点数增长，建议每 profile ≤10 站点。

## 2. 回调地址与重定向

- `redirect_uri` / `post_logout_redirect_uri` 一律由 `server.public_base_url` 推导，**完全不信任 Host 头**；
  未配置时回退 `trusted_hosts` 白名单（`BFF_ENV=prod` 强制二者至少其一）；
- OIDC 客户端缓存键包含 `(provider, base_url)`，规避“首个调用者固化回调地址”的粘性污染；
- 登录后的回跳地址做同源校验：拒绝反斜杠 / `%5C`、控制字符、非 `/` 开头、协议相对与绝对外链；
- 回调路径按 provider 配置动态注册（默认 `/auth/callback`），配置校验拒绝与保留路径冲突。

**验证**：`tests/test_public_base_url.rs`（含伪造 Host 并发用例）、HTTPS + LB 全链路 E2E（`deploy/https/e2e.sh`）。

## 3. 管理面

| 能力 | 说明 |
| --- | --- |
| 网络隔离 | 独立端口；IP 白名单（每请求实时读取；生产收敛到运维网段，禁止公网 Ingress） |
| 认证 | `X-Admin-Token` / `Bearer`；SHA-256 摘要**常量时间比较**；失败按来源 IP 限流（默认 30 次/分钟 → 429） |
| 会话撤销语义 | 共享会话下 `DELETE /admin/api/sessions/:id` 清掉该会话的**全部站点 token** = **全站踢出**；管理台按钮已明示“终止该用户在所有站点的会话” |
| 测试端点防呆 | `enable_test_endpoints` 在 `BFF_ENV=prod` 下强制关闭 |
| 安全响应头 | CSP / X-Frame-Options / nosniff / Referrer-Policy / HSTS 覆盖管理面 |
| 前端凭据存放 | Admin UI 的 token 使用 `sessionStorage` + 内存（不落 `localStorage`） |
| 审计 | 管理写操作结构化审计（操作者、来源 IP、方法 / 路径 / 状态）；配置变更记录变更摘要 |

**验证**：集成测试覆盖令牌校验路径与失败限流；管理白名单与来源 IP 解析有单元与集成双层用例。

## 4. 密钥与配置管理

- `bff_secret`（AES 主密钥）与各项 `client_secret` / 管理 token 均通过环境变量或密钥管理注入；
- 配置导出**自动脱敏**：密钥字段输出 `***` 哨兵；导入 / 编辑回写时哨兵回填真实值（不会覆盖为哨兵）；
- `bff_secret` **拒绝热更新**：检测到密钥变更的导入直接报错（提示重启），避免“配置显示新密钥、实际用旧密钥”的静默分裂；
- 持久化文件（`runtime.yaml`）为**脱敏快照**，原子写入（临时文件 + rename）；
- `BFF_ENV=prod` 启动防呆（fail-fast）：拒绝弱管理口令（<32 字符）、内存 provider、跳过验签、
  `SameSite=None`、未固定对外地址等不安全组合。

**验证**：`tests/test_admin_config_import_export.rs`、`tests/test_config_persistence.rs`（落盘无明文密钥、重启恢复、外部变更收敛）。

## 5. 输入与边界

| 能力 | 说明 |
| --- | --- |
| 体量上限 | 请求体默认 10 MiB；代理响应 64 MiB（流式累计硬上限 + Content-Length 快速拒绝）；管理 API 8 MiB |
| 脚本沙箱 | QuickJS：内存 / 栈 / 时长上限（默认 2s）；`spawn_blocking` 隔离，不阻塞运行时 |
| 过度收集防护 | 输入映射的 `env` / `header` 上下文仅注入**被显式引用**的键；敏感请求头默认过滤 |
| CORS | 默认拒绝跨域；仅显式白名单放行（`permissive` 仅限本地开发） |
| 响应头过滤 | 代理与 SSE 统一剥离 hop-by-hop / `access-control-*`；默认剥离 `set-cookie`（路由级可显式放开） |
| 错误文案 | 对外错误仅保留类别描述；细节进日志，通过 `x-request-id` 关联 |

**验证**：映射与边界行为均有集成测试（`tests/test_route_dispatch.rs`、`tests/test_mapping_engine.rs` 等）。

## 6. 限流与熔断

- **全局限流**按真实客户端 IP 建桶（统一 XFF 解析：按 nginx `proxy_add_x_forwarded_for` 语义，左侧伪造条目被忽略）；
  429 响应带 RFC 6585 `Retry-After`；
  ⚠️ 实现注意：`tower-governor` 0.4.x 的 `per_second` 是周期语义（每 N 秒 1 个），
  已显式换算为「每秒 N 个」（有回归测试锁定，升级依赖时勿回退为直接透传）；
- **认证端点限流**：按来源 IP + 路径前缀独立令牌桶（与 IdP 侧账号锁定互补）；
- **熔断**：滚动窗口失败计数（成功不清零，间歇故障可触发）+ 半开**单探针**（含探针超时复位）；
  SSE 按流终态计（正常结束 = 成功）；
- **WebSocket**：仅受控路由可升级；按 `auth_required` 强制鉴权并向上游注入 Bearer；
  握手超时、空闲超时、心跳与消息大小上限（超限以 1009 关闭）。

**验证**：`tests/test_ip_rate_limit.rs`、`tests/test_spa_serving.rs`（限流补液速率回归）、`tests/test_ws_tunnel.rs`（9 用例）。

## 7. 供应链与交付物

- **依赖审计门禁**：CI `audit` job 执行 RustSec 审计；例外清单（`.cargo/audit.toml`）每一条均含
  **引入链 + 影响界定 + 整改计划**，禁止静默忽略。当前例外仅 3 条：
  `serde_yaml`（停维，figment 锁定）、`rustls-pemfile`（停维，tonic/OTLP 栈）、
  `rsa` Marvin（针对私钥运算的侧信道；生产仅公钥验签，私钥运算不可达）；
- **纯 rustls 栈**：`openssl-sys` / `native-tls` 在依赖图中零命中；
- **旧栈清零**：openidconnect 4.0 / oauth2 5 / reqwest 0.12 迁移后，h2 0.3 / rustls 0.21 /
  rustls-webpki 0.101 / hyper 0.14 / idna 0.3 全部移除；出网统一 HTTP/1.1（行为确定性优先）；
- **容器**：最小运行时镜像（无 apt 依赖）；以数值 UID `10001` 非 root 运行；
  K8s 清单配 `nonroot` + `seccomp` + NetworkPolicy；
- **发布产物**：多架构镜像 + Linux 二进制；release profile 开启 `lto` / `strip`。

## 8. 验证与测试资产

| 资产 | 内容 |
| --- | --- |
| `cargo test`（CI 门禁） | 200+ 用例：OIDC 真实验签攻击拒绝、WS 隧道、限流语义、配置持久化、映射与分发、Telemetry 契约等 |
| 覆盖率门禁 | `cargo-llvm-cov` ≥75% lines（棘轮策略逐步上调） |
| Keycloak 26 真实 IdP 契约 E2E | `deploy/keycloak/e2e-keycloak.sh`：discovery / 登录（PKCE + RS256） / Bearer 注入 / Redis 跨重启会话 / 刷新 / 登出，8 组断言 |
| HTTPS + LB 全链路 E2E | `deploy/https/e2e.sh`：nginx TLS 终止下的登录 / 回调 / 会话 / 登出，含伪造 Host 拒绝验证 |
| SLO / 容量基线 | k6 压测（`benchmark/`）：单实例 ≥10.4k QPS、0 错误；详见 [deployment.md](deployment.md) §5 |

## 9. 已知边界与上线前自评

- 未进行**外部渗透测试**；上线前建议安排（重点：OIDC 回调、`/pipeline` 入口、代理注入、管理面）；
- 生产域名 HTTPS 终验与 IdP 侧注册值核对由部署方完成；
- 示例配置中的 `http://localhost` / 占位密钥仅用于本地联调，**不可照抄上线**；
- 管理端口不得暴露公网；请完成 [deployment.md](deployment.md) §6 安全清单逐项核对。
