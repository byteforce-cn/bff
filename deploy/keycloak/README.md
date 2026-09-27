# Keycloak 真实 IdP 契约验证

用 **Keycloak 26**（Docker）替代仓库自带的 Mock IdP / Spring AS，验证 BFF 对**真实、
非 Spring Authorization Server 的 IdP** 的 OIDC 契约兼容性（审计 v2「上线前 Should 项」：
真实 IdP 兼容性验证）。

与 Mock IdP（`examples/mock_idp.rs`、未签名 id_token、自动授权）的关键差异：

| 维度 | Mock IdP / Spring AS | Keycloak 26（本目录） |
| ---- | ---- | ---- |
| id_token 签名 | `none`（跳过验签）/ 自签 | **真实 RS256 + JWKS 验签** |
| scope 语义 | 忽略 | 严格校验（重复 scope 可能 `invalid_scope`） |
| 登录交互 | 自动回跳 | **真实登录页表单 + 会话 Cookie** |
| 登出 | 自定义路径 | discovery `end_session_endpoint` + `id_token_hint` + 注册的 `post_logout_redirect_uris` |
| 刷新 | 简单轮换 | 真实 refresh_token grant（机密客户端 client_secret_post） |

## 拓扑

```text
curl / 浏览器（宿主，--resolve host.docker.internal→127.0.0.1）
  ──https:9443──▶ nginx（TLS 终止）
                    └──▶ bff:8080（BFF_ENV=prod 全量防呆 + Redis provider，
                                   public_base_url=https://localhost:9443）
                          ├─ OIDC ──▶ http://host.docker.internal:8180（Keycloak 26）
                          └─ /api/echo ──▶ upstream-echo:8081（回显 Authorization）
```

## 一键验收

```bash
bash deploy/keycloak/e2e-keycloak.sh                 # 复用 bff:local 镜像
FORCE_BUILD=1 bash deploy/keycloak/e2e-keycloak.sh   # 代码有改动时重建镜像
KEEP_STACK=1  bash deploy/keycloak/e2e-keycloak.sh   # 保留栈现场（排查用）
```

脚本自动完成：生成生产防呆所需密钥 → 启动全栈 → 等待就绪 → 全链路断言 → 清理。

**断言的验证点**

1. discovery 契约：issuer 与 BFF 配置一致、`end_session_endpoint`、PKCE S256、
   `client_secret_post`；BFF 容器内（经 host-gateway）真实 discovery（admin verify 端点）；
2. `/login` 重定向：`redirect_uri` 恒为 `public_base_url` 推导值（伪造 Host 不污染）、
   PKCE S256、scope 去重（`openid+profile+email`）；
3. 真实登录：Keycloak 登录页 → 凭据提交 → 回调换码 → **RS256/JWKS 验签 + nonce/state**
   → `/api/session` logged_in=true；管理端会话列表登记 keycloak 会话；
4. Bearer 注入：`/api/echo` 上游实际收到 Keycloak 签发的 access token（校验 iss/azp/typ）；
5. Redis 会话：`docker restart` bff 容器后登录态不丢；
6. 令牌刷新：realm AT 寿命 90s + skew 60s → 登录 ~35s 后 SWR 后台刷新，
   上游观察到 token 轮换 + Keycloak `REFRESH_TOKEN` 事件；
7. RP-Initiated Logout：`end_session_endpoint` + `id_token_hint` 回跳成功、
   本地会话清除 + Keycloak `LOGOUT` 事件。

## 手动分步

```bash
# 1) 密钥（无默认值，缺失时 compose 直接报错）
export BFF_ADMIN_TOKEN=$(openssl rand -hex 32)
export BFF_SECRET=$(openssl rand -hex 32)
export BFF_SECRET_SALT=$(openssl rand -hex 16)

# 2) 启动全栈（根 + https 叠加 + Keycloak 叠加）
docker compose -f docker-compose.yml \
  -f deploy/https/docker-compose.https.yml \
  -f deploy/keycloak/docker-compose.keycloak.yml up -d

# 3) Keycloak 管理控制台（可选）
#    http://127.0.0.1:8180 （admin / admin）
#    realm: bff，client: bff（secret: bff-keycloak-secret），用户: bff-user / bff-pass

# 4) 浏览器走一遍（可选）：宿主机 /etc/hosts 增加
#    127.0.0.1 host.docker.internal
#    然后打开 https://localhost:9443/login（自签证书需信任）
```

## 已知本地特性（非生产）

- `host.docker.internal:8180` 是"浏览器/BFF 容器共用同一 issuer 字面量"的本地手段：
  BFF 容器经 `extra_hosts`(host-gateway) 访问，宿主机 curl 经 `--resolve`；
  真实部署应使用统一的 DNS 名（如 `sso.example.com`）；
- Keycloak 以 `start-dev` 运行、`sslRequired=none`、bootstrap 口令与测试凭据均为
  **明文测试值**，8180 监听所有接口——**仅限隔离的开发/验收环境**；
- realm 配置见 `realm-bff.json`（`accessTokenLifespan=90` 是为刷新断言压缩的寿命，
  生产按需调整）；realm 事件保留 2 小时供断言查询。
