# RFC 8693 Token Exchange（面向上游的令牌交换）

> 实现：`src/server/token_exchange.rs`；配置模型：`src/config.rs::TokenExchangeConfig`；
> 接入点：代理路由 `RouteTypeConfig.token_exchange`（HTTP/SSE 生效；WS 本期不执行，配置时会告警）。
> 本文档补齐配置导入 / 导出此前缺失的设计说明。

## 1. 目标与语义

BFF 持有的是**面向 BFF 自身**的会话 access token；上游服务通常要求
**面向其资源受众**的令牌。本能力以 RFC 8693 交换为代理请求注入面向上游的
`Authorization: Bearer`，并在 BFF 侧完成缓存与失败语义治理。

```
浏览器 --(会话 cookie)--> BFF --(token-exchange)--> 授权服务器
                            |  subject_token = 会话 access_token
                            |  audience/scope 收窄
                            v
                    面向上游的 access_token --> 注入代理请求 --> 上游服务
```

## 2. 配置

```yaml
routes:
  - path: "/upstream-admin/v1"
    methods: ["GET", "POST"]
    auth_required: true            # 交换以会话 token 为前提，必须为 true
    type: proxy
    config:
      upstream: "http://upstream:9091"
      token_exchange:
        token_endpoint: "http://as:9000/oauth2/token"  # 可省：回退 provider discovery（结果缓存 10min）
        client_id: "bff"
        client_secret: "${BFF_AS_CLIENT_SECRET:change-me}"  # 建议环境变量注入；导出自动打码
        client_auth_method: "client_secret_basic"      # client_secret_basic | client_secret_post
        audience: ["admin-api"]                        # 可重复；不配置则交换无收窄效果（启动告警）
        scope: "admin.api"                             # 可选
        cache_ttl: "60s"
```

字段语义与校验（`validate()`）：

| 字段 | 说明 |
| --- | --- |
| `token_endpoint` | 缺省时回退会话 provider 的 discovery（带 10 分钟缓存） |
| `client_auth_method` | `client_secret_basic`（默认，HTTP Basic）或 `client_secret_post`（表单字段）；`client_secret` 为空按 public client 处理（不认证） |
| `subject_token_type` | 缺省 `urn:ietf:params:oauth:token-type:access_token` |
| `requested_token_type` | 缺省同为 access_token；响应必须匹配合法 token-type 前缀 |
| `audience` / `scope` | 可重复 / 单值；两者均空时启动告警（交换无收窄） |
| `cache_ttl` | 缓存上限；实际 TTL 见 §4 |
| `actor_token_type` | 预留（委托场景本期无来源），配置即启动告警且不发送 |

## 3. 请求/响应契约

请求（`application/x-www-form-urlencoded`）：

```
grant_type=urn:ietf:params:oauth:grant-type:token-exchange
subject_token=<会话 access_token>
subject_token_type=urn:ietf:params:oauth:token-type:access_token
requested_token_type=urn:ietf:params:oauth:token-type:access_token
audience=<可重复>
scope=<可选>
```

响应：仅接受 `token_type=Bearer`；`issued_token_type` 必须是合法
`urn:ietf:params:oauth:token-type:*`；`access_token` 必填；`expires_in` 缺省视为
3600s（用于 TTL 截断）。

## 4. 缓存与并发（single-flight）

- **键**：`bff:token_exchange:{session_id}:{cfg_fp16}:{subject_fp16}`
  - `cfg_fp`：`token_endpoint|client_id|audience|scope|requested_token_type` 的 SHA-256 前 16 位——
    同 endpoint 不同 audience/scope 不串用；
  - `subject_fp`：会话 access_token 的 SHA-256 前 16 位——会话刷新后键自动失效；
- **值**：`TokenExchangeResult` JSON **AES-256-GCM 加密**后写入 `CacheProvider`
  （缓存中不出现上游明文令牌）；
- **TTL** = `min(cache_ttl, expires_in − 30s, 300s)`（后端内存上限），下限 1s；
- **single-flight**：miss 时经 `LockProvider` 抢 `bff:token_exchange_lock:{key}`：
  - 持锁者执行交换并写缓存；
  - 未持锁者最多等待 5×100ms 重读缓存，仍 miss 则自行交换（避免慢持有者拖死）；
- **失效**：会话登出 / 管理端撤销会话时按 `bff:token_exchange:{sid}:` 前缀清理。

## 5. 失败语义与重试

| 上游错误码 | 分类 | BFF 行为 | 客户端可见 |
| --- | --- | --- | --- |
| `invalid_grant` / `invalid_token` | `InvalidSubject` | **刷新会话令牌后重试一次**；仍失败终止 | 401 |
| `access_denied` / `invalid_scope` | `Denied` | 不重试 | 401 |
| `invalid_client` / `invalid_request` / `unauthorized_client` | `ClientConfig` | 不重试（BFF 配置/实现问题） | 500 |
| `unsupported_grant_type` / 网络错误 / 5xx | `Upstream` | 不重试 | 502 |
| 其他非 2xx | 按 HTTP 语义兜底（401→ClientConfig、5xx→Upstream） | — | — |

指标：`bff_token_exchange_total{result=cache_hit|error}`、
`bff_token_exchange_error_total{error=invalid_subject|denied|client_config|upstream}`、
`bff_token_exchange_duration_seconds`（直方图）。

## 6. 安全注意

- `client_secret` 支持 `${ENV:default}` 注入；配置导出自动打码为 `***`，
  导入回环按 `route.path` 回填；
- 交换缓存加密密钥即 `BFF_SECRET`（轮换 = 全员重登，见部署文档迁移预警）；
- 该能力放大 BFF 的令牌面：务必配置 `audience`/`scope` 收窄，遵循最小权限；
- 上游 401 时代理层会 `force_refresh` 会话令牌并**重下一轮交换**（subject 变化 → 键变化 → 重新交换）。

## 7. 测试

- `tests/test_token_exchange.rs`：缓存命中/回源、single-flight、`invalid_grant`
  刷新重试、错误映射、导出打码（t10）、并发等；
- 本地手工验证：`routes.yaml` 中取消注释示例路由，指向可用授权服务器即可。
