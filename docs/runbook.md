# BFF Runbook（告警处置手册）

> 告警定义见 `deploy/prometheus/bff-alerts.yaml`；仪表盘见 `deploy/grafana/bff-dashboard.json`。
> 通用前置：确认变更窗口（`kubectl -n bff rollout status deploy/bff`）与最近发布记录。

---

## BffDown

**含义**：Prometheus 抓取不到实例 / 实例不存活。

1. `kubectl -n bff get pods -l app.kubernetes.io/name=bff` 看 Pod 状态与重启次数；
2. 崩溃循环：`kubectl -n bff logs deploy/bff --previous | tail -100`，
   常见原因：密钥缺失（弱口令已被 prod 防呆拒绝）、配置校验失败、端口占用；
3. 若仅单 Pod：确认是否正在滚动发布（`kubectl rollout status`）；
4. 全挂：先回滚 `kubectl -n bff rollout undo deploy/bff`，再排查。

## High Error Rate（5xx > 5%）

1. 仪表盘「请求速率（按状态类）」定位 5xx 集中路径；
2. 若集中在上游代理：查「上游延迟 P95」「熔断打开」面板 → 见 Circuit Breaker 条目；
3. 若集中在 `/auth/callback`：检查 IdP 可用性与 discovery（`admin/api/v1/oidc/providers/{id}/verify`）；
4. 若为配置变更后出现：回滚配置——
   管理端 import 上一版导出文件，或恢复 `runtime.yaml` 后触发 watcher/滚动重启；
5. 保留证据：`traceparent` 响应头 + 日志中同 trace 的服务端链路。

## High Latency（P95 > 1s）

1. 看路径维度（`path` 模板标签）区分：上游慢 / 编排脚本慢 / 自身排队；
2. 上游慢：查目标 upstream 的 P95；考虑临时提高该路由 `config.timeout` 或用熔断兜底；
3. 自查连接池：`http_client.pool_max_idle_per_host`（需重启）与 HPA 水位；
4. 大响应场景确认未触发 `max_response_bytes` 截断错误。

## Circuit Breaker（熔断打开）

1. 面板确认打开的 upstream（键为路由 path）；
2. 检查该上游健康（网络、证书、限流），必要时上游侧扩容/回滚；
3. **不要**在故障中临时调大 `failure_threshold` 掩盖问题；
   修复上游后熔断器会在冷却期（`open_duration`）后放行**单探针**自动恢复；
4. 若属误伤（如计划内维护），按变更流程短暂调整阈值（需重启生效）。

## Not Ready（readiness 失败/无可用副本）

1. `kubectl describe pod` 看探针失败原因；
2. 手动复现：`curl -s http://<pod>:8080/ready | jq`；
   `upstreams_unreachable > 0` → 按 `health.upstreams` 清单逐个排查；
3. 上游抖动导致全副本同时 503：确认 `health.allow_degraded` 语义是否符合期望
   （当前为 false 即严格模式）；
4. 探针缓存 1s（`health.cache_ttl`），抖动需持续 > 缓存时长才会反映。

## Rate Limit（429 激增）

1. 确认来源：面板「认证限流 429 / 锁竞争」；日志 `auth_rate_limit ... 429`（含 IP）；
2. 正常业务高峰：按 SLO 基线调 `auth_rate_limit.per_ip`（热生效）；
3. 疑似撞库：保持限流并升级 IAM 侧账号锁定策略；核查 `X-Forwarded-For` 解析
   （`trusted_proxies` 是否与入口一致，避免全站误伤或伪造绕过）；
4. 全局限流 429：确认 `rate_limit.skip_path_prefixes` 是否覆盖 SPA 静态资源；
   排查响应头 `x-ratelimit-after`（秒）估算恢复时间；
5. 全局限流参数（`rate_limit.per_second/burst_size`）在**启动期固化**，调整后需滚动重启；
   `per_second` 语义 = 每秒补液令牌数（容量与参数反推见 `docs/production-deployment.md` §SLO），
   压测验证方法见 `benchmark/README.md`。

## Token Exchange

1. `bff_token_exchange_error_total{error=...}` 定位分类：
   `invalid_grant/invalid_token` → 会话刷新/IdP 授权策略变化；
   `denied` → 权限/audience/scope 配置；
   `client_config` → client_id/secret/token_endpoint 配置；
   `upstream` → 授权服务器故障；
2. 会话登出后缓存已随会话清理（R17）；如怀疑残留，可重启或等待 TTL 过期；
3. discovery 结果缓存 10 分钟；更换 IdP token endpoint 后需等待或重启。

---

## 配置相关应急操作

| 操作 | 步骤 |
| --- | --- |
| 回滚最近一次管理端配置 | 用变更前的 `config/export` 文件重新 import（`***` 哨兵会保留现网密钥） |
| 强制放弃运行时覆盖 | 停止实例 → 移除/备份 `runtime.yaml` → 重启（回落到 base/env 配置） |
| 密钥轮换 | 走 §production-deployment.md「迁移预警」：与 Session 迁移合并窗口执行，重启生效，全员重登 |
| 恢复单实例形态 | 临时置 `provider.*=memory` **仅限已声明风险的单副本环境**；prod 防呆会拒绝，需同时改环境标记（不建议） |
