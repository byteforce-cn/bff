# BFF 基准负载测试

## 前置依赖

```bash
# 安装 k6（支持 Linux / macOS / Windows）
# https://grafana.com/docs/k6/latest/set-up/install-k6/

# macOS
brew install k6

# Debian/Ubuntu
sudo apt-get install -y ca-certificates gnupg
echo "deb [signed-by=/usr/share/keyrings/grafana.gpg] https://apt.grafana.com stable main" | sudo tee /etc/apt/sources.list.d/grafana.list
sudo apt-get update && sudo apt-get install k6

# 或使用 Docker
docker run --rm -i --network host grafana/k6 run - < k6-load-test.js
```

## 测试场景

| 场景 | 说明 | VU | 时长 |
|------|------|:--:|:----:|
| `smoke` | 冒烟测试，验证所有端点可达 | 1 | 30s |
| `baseline` | 阶梯式加压，找单实例 QPS 上限 | 1→200 | 2.5min |
| `capacity` | 容量拐点：比 baseline 更重的阶梯（含高温段） | 100→800 | 2.5min |
| `stress` | 压力测试，观察高负载下降级行为 | 100→200 | 5.5min |
| `endurance` | 耐久测试，检测内存泄漏 | 50 | 10min |

> 思考时间：`smoke`/`endurance` 为 0.5s/次（贴近真实用户）；`baseline`/`capacity`/`stress` 降至 0.05s 以逼近容量上限。

## 运行

```bash
# 确保 BFF 和上游服务已启动
# Terminal 1: cargo run
# Terminal 2: cd fakesvc && mvn spring-boot:run

# 冒烟测试（快速验证）
k6 run k6-load-test.js

# 基线测试（找 QPS 上限）
k6 run --env SCENARIO=baseline k6-load-test.js

# 压力测试
k6 run --env SCENARIO=stress k6-load-test.js

# 耐久测试
k6 run --env SCENARIO=endurance k6-load-test.js

# 指定目标地址
k6 run --env BASE_URL=http://192.168.1.100:8080 --env SCENARIO=baseline k6-load-test.js
```

或使用一键脚本：

```bash
chmod +x run.sh
./run.sh smoke
./run.sh baseline
./run.sh stress
./run.sh endurance
./run.sh all       # 依次运行所有场景
```

或用 Makefile（Docker 版 k6，无需本机安装）：

```bash
make bench                          # 默认 smoke
SCENARIO=capacity make bench        # 指定场景
COOKIE="BFF_SESSION=..." SCENARIO=baseline make bench   # 认证路径

## 本地复现环境（推荐）

无需 fakesvc / Java，使用仓库自带的最小资产即可测得可比的 BFF 自身容量：

```bash
# 1) 上游（nginx，所有路径 200 JSON；host 网络占用 9091）
docker run -d --name bff-loadtest-upstream --network host \
  -v "$PWD/benchmark/upstream-nginx.conf:/etc/nginx/conf.d/default.conf:ro" nginx:1.27-alpine

# 2) Redis（会话/缓存/锁，生产同构）
docker run -d --name bff-redis -p 127.0.0.1:6379:6379 redis:7-alpine

# 3) Mock IdP（登录链路；release 示例二进制）
MOCK_IDP_ISSUER=http://localhost:9090 cargo run --release --example mock_idp &

# 4) BFF（release；Redis provider；压测放宽全局限流以测引擎容量）
BFF_PROVIDER__SESSION_STORE=redis BFF_PROVIDER__CACHE=redis BFF_PROVIDER__LOCK=redis \
BFF_PROVIDER__REDIS_URL=redis://127.0.0.1:6379 BFF_SESSION__SECURE=false \
BFF_RATE_LIMIT__PER_SECOND=100000 BFF_RATE_LIMIT__BURST_SIZE=100000 \
./target/release/bff
```

> `providers.yaml` 需指向 Mock IdP（`issuer_url: http://localhost:9090` +
> `insecure_skip_id_token_verification: true`）；可用 `BFF_CONFIG_DIR` 指向压测专用配置副本。

### 认证路径（代理/编排路由需要会话）

```bash
# 登录（Mock IdP 自动授权回跳），获取会话 Cookie
curl -s -c /tmp/bff-jar.txt -L 'http://127.0.0.1:8080/login?provider=iam' -o /dev/null
curl -s -b /tmp/bff-jar.txt http://127.0.0.1:8080/api/session   # {"logged_in":true}

# k6 携带 Cookie（脚本会注入到所有请求）
COOKIE=$(awk '/BFF_SESSION/{print $6"="$7}' /tmp/bff-jar.txt)
docker run --rm -i --network host -v "$PWD/benchmark":/bench -w /bench grafana/k6 \
  run --env SCENARIO=baseline --env COOKIE="$COOKIE" k6-load-test.js
```

## 输出

- 控制台输出测试摘要（QPS、延迟分布、错误率）
- `results/benchmark-{scenario}-{timestamp}.json` — 完整 k6 原始数据

## 测试端点

| 端点 | 说明 | 类型 |
|------|------|:----:|
| `GET /live` | 存活检查 | Fast path |
| `GET /ready` | 就绪检查（探测上游） | Dependency check |
| `GET /api/echo` | Pipeline 引擎验证 | Script execution |
| `GET /api/health` | 静态响应 | Static |
| `GET /api/users` | 代理到 fakesvc | Proxy |
| `GET /api/orders` | 代理到 fakesvc | Proxy |

> 注：`/api/users` 和 `/api/orders` 需要认证 session，未认证时返回 302/401，指标中会体现。
> 如需测试认证路径，先通过 OIDC 流程获取 session cookie 后传入 `Cookie` 头。

## 指标说明

| 指标 | 说明 |
|------|------|
| `http_req_duration` | HTTP 请求端到端延迟 |
| `bff_pipeline_duration_ms` | Pipeline 执行耗时（自定义） |
| `bff_proxy_duration_ms` | 代理请求耗时（自定义） |
| `bff_errors_by_endpoint` | 各端点错误计数（自定义） |
| `bff_error_rate` | 错误率（自定义） |

## 实测基线（2026-09-27）

> 环境：8 vCPU（i7-1165G7）· 31GB RAM；BFF release 本地进程（Redis 会话/缓存/锁，
> 经 Mock IdP 真实登录）+ nginx 上游 + k6（均 Docker host 网络）——k6/Redis 与 BFF
> 共享同一台机器，数值偏保守；全局限流在压测中放宽（100000/s）以测引擎容量。
> 原始数据：`results/benchmark-{smoke,baseline,capacity}-*.json`。

| 场景 | 请求数 | QPS | 错误率 | avg | p50 | p90 | p95 | p99 | max |
| ---- | -----: | --: | :----: | --: | --: | --: | --: | --: | --: |
| smoke（1 VU） | 80 | 2.7 | **0.00%** | 1.14ms | 0.96 | 1.93 | 2.00 | 2.36 | 3.05 |
| baseline（≤200 VU） | 311,886 | 2,078 | **0.00%** | 0.93ms | 0.57 | 1.88 | 2.89 | 6.09 | 29.05 |
| capacity（≤800 VU） | 1,572,720 | **10,464** | **0.00%** | 9.45ms | 2.53 | 29.25 | 39.41 | 63.80 | 838 |

分端点 p95（baseline / capacity）：

| 端点 | 类型 | p95（baseline） | p95（capacity） |
| ---- | ---- | ---: | ---: |
| `/live` | 存活（无依赖） | 1.27ms | 6.60ms |
| `/ready` | 就绪探测（缓存 1s） | 2.53ms | 15.79ms |
| `/api/health` | 静态响应 | 2.45ms | 30.52ms |
| `/api/echo` | Pipeline + QuickJS 脚本 | 3.95ms | 31.95ms |
| `/api/users` | 代理 + Redis 会话 + Bearer 注入 | 3.64ms | 56.48ms |
| `/api/orders` | 代理 + Redis 会话 + Bearer 注入 | 2.99ms | 52.88ms |

结论（对照部署文档 SLO 目标 P95 ≤ 500ms）：

- **单实例 ≥ 10,464 QPS** 且 0 错误、0 丢弃（dropped_iterations=0）；共享 8 线程环境下仍未出现错误
  拐点（延迟随负载上升，但无失败/无熔断/无限流误伤）。
- 最重的代理路径在峰值仅 p95 ≈ 53–56ms，**对 SLO 有 ~9× 余量**；脚本/Pipeline 路径 p95 ≈ 32ms。
- 结果用于反推限流参数（见 `docs/production-deployment.md` §SLO）：单 IP 50rps 默认在实测能力内
  留出两个数量级余量，适合作为防滥用阈值而非容量阈值。

### ⚠️ 压测暴露的真实缺陷（已修复）

首次 baseline 全场景 **68.11% 错误（429）**（313,320 请求仅 ~100k 通过 = burst 耗尽后全部拒绝）：
根因为全局限流误用 `tower-governor 0.4.x` 的 `per_second`（其语义是「每 N 秒补 1 个令牌」的
周期，而非「每秒 N 个」）—— 配置 50/s 实际退化为**每 50 秒 1 个请求**。
修复：显式换算补液周期 `1s / per_second`（`src/middleware/rate_limit_skip.rs`），
并补充单测 + 集成回归测试（`test_spa_serving.rs::global_rate_limit_refill_uses_per_second_rate`）。
