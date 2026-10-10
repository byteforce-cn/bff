# 本地双子域 SSO 验收（nginx 反向代理）

用一条 nginx 作为本地 LB，把 `app1.localhost` / `app2.localhost` 代理到 BFF 的两个站点端口
（`127.0.0.1:8081` / `8082`），用于验收跨站 SSO、站点隔离、伪造 Host 拒绝（§11.2、§14-5）。

> 为什么需要子域而不是 `localhost:8081/8082`：host-only Cookie 按“主机”而非端口共享，
> Domain cookie 语义在纯 `localhost:<port>` 下无法验证。跨站 SSO 必须在不同**主机**下观察。

## 组成

| 文件 | 说明 |
|---|---|
| `nginx.conf` | 两个 `server` 块：`app1.localhost → 127.0.0.1:8081`、`app2.localhost → 127.0.0.1:8082`，`proxy_set_header Host $host;` + `X-Forwarded-Proto $scheme;` |
| `config.example.yaml` | 两站点 dev 配置示例（无顶层 `server.public_base_url`；站点 `server_names` + `public_base_url`；`session.cookie_domain` + `allow_unmanaged_subdomains`） |

## 前置

- `*.localhost` 默认解析到 `127.0.0.1`（Linux/macOS 通用；浏览器与 curl 均适用）。
- nginx 需绑定 80 端口（用 `sudo` 运行，或授予 `CAP_NET_BIND_SERVICE`）。
- mock IdP：示例 provider 指向 `http://127.0.0.1:9090`。

## 启动

```bash
# 1) 启动 mock IdP（终端 A）
cargo run --release --example mock_idp          # 监听 127.0.0.1:9090

# 2) 以两站点配置启动 BFF（终端 B，dev 环境）
mkdir -p /tmp/bff-multisite
cp deploy/multi-site/config.example.yaml /tmp/bff-multisite/base.yaml
BFF_ENV=dev BFF_CONFIG_DIR=/tmp/bff-multisite cargo run --release
#   站点 app1/app2 分别监听 127.0.0.1:8081/8082；admin 监听 8443

# 3) 启动 nginx（终端 C，需 80 端口权限）
sudo nginx -c "$PWD/deploy/multi-site/nginx.conf" -g 'daemon off;'
```

## 手工验收（curl）

```bash
# 探针路径站点无关且豁免 Host 校验：两个子域都应 200
curl -i http://app1.localhost/live
curl -i http://app2.localhost/live

# 伪造 Host → 421 Misdirected Request（§6.3）。
# nginx 的 default_server 会把不匹配的 Host 原样透传给 app1 站点，BFF 拒绝：
curl -i -H 'Host: evil.example.com' http://app1.localhost/api/session
# （直连 BFF 亦可复现：curl -i -H 'Host: evil.example.com' http://127.0.0.1:8081/api/session）

# 跨站 SSO（§14-2）：在 app1 登录后，同一 Cookie jar 访问 app2 应无需登录页即可认证。
# 跟随 mock IdP 的自动回跳完成 app1 登录：
curl -s -c /tmp/bff-multisite/jar -L 'http://app1.localhost/login?provider=app1' -o /dev/null
# 会话 Cookie 名为 BFF_SESSION_V2、Domain=.localhost，浏览器/curl 会带到 app2：
curl -s -b /tmp/bff-multisite/jar http://app1.localhost/api/session   # {"logged_in":true,"provider":"app1"}
curl -s -b /tmp/bff-multisite/jar http://app2.localhost/api/session   # 共享会话已建立（cookie 命中）
```

> `/api/session` 的 `logged_in` 为**站点维度**：app2 首次访问会以共享 Cookie 走 app2 自己的
> provider 静默续登（§7.3）。浏览器中打开 `http://app1.localhost/login` 完成登录后，直接访问
> `http://app2.localhost/` 可观察“无登录页”的跨站 SSO。

## 迁移演练（§11.3，两步发布）

1. **行为中立发布**：先部署多站点能力二进制，配置保持**无 `sites`**（legacy 路径），行为与升级前一致。
2. **切换配置**：新增 `sites`（原站点沿用名 `default` 与原业务端口）、`session.cookie_name` 轮换为
   `BFF_SESSION_V2`、`cookie_domain`、每站点 `public_base_url`/`server_names`，并显式
   `allow_unmanaged_subdomains: true`。**推荐该次发布用 `strategy: Recreate`**（见
   `deploy/k8s/deployment.yaml` 注释）：避免新旧 Pod 各写不同 Cookie 名导致同一用户多次重认证。

轮换后旧的 host-only `BFF_SESSION` Cookie 仍会被浏览器发送，但被新代码**忽略**并随 Max-Age 自然过期。
该行为有集成测试守护：`tests/test_multi_site_migration.rs`（旧 Cookie → 新站点 `logged_in=false` 且不 5xx；
新状态重新登录正常；可重复执行）。
