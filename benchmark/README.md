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
| `stress` | 压力测试，观察高负载下降级行为 | 100→200 | 5.5min |
| `endurance` | 耐久测试，检测内存泄漏 | 50 | 10min |

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

## 基线参考值（待实测）

| 指标 | 目标值 |
|------|:------:|
| `/live` p95 | < 10ms |
| `/api/echo` p95 | < 50ms |
| `/api/users` (代理) p95 | < 500ms |
| 单实例 QPS 上限 | TBD |
| 错误率 | < 0.1% |
