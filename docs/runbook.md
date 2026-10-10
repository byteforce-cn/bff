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
2. 手动复现：`curl -s http://<pod>:8080/ready | jq`（多站点探针打任一业务端口，如 `app1:8081`）；
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
   `per_second` 语义 = 每秒补液令牌数（容量与参数反推见 `docs/deployment.md` §SLO），
   压测验证方法见 `benchmark/README.md`。

## Token Exchange

1. `bff_token_exchange_error_total{error=...}` 定位分类：
   `invalid_grant/invalid_token` → 会话刷新/IdP 授权策略变化；
   `denied` → 权限/audience/scope 配置；
   `client_config` → client_id/secret/token_endpoint 配置；
   `upstream` → 授权服务器故障；
2. 会话登出后缓存已随会话清理；如怀疑残留，可重启或等待 TTL 过期；
3. discovery 结果缓存 10 分钟；更换 IdP token endpoint 后需等待或重启。

---

## 多站点（Multi-site）

### 配置导入返回 `requires_restart`

管理端 import（`POST /admin/api/config/import`，版本化路径为 `POST /admin/api/v1/config/import`）对含**启动物化字段**变更的配置返回
`{"status": "requires_restart", "hot_applied": [...], "requires_restart": [...]}`，**不替换运行配置、
不落盘**（设计 §5.6）：

1. 阅读 `requires_restart` 清单（字段路径，如 `sites[app1].port`、
   `session_profiles[default].cookie_name`）；
2. 这些变更只能通过**重启 / 滚动发布**生效——涉及 `sites[].port` 时先同步 Deployment / Service /
   Ingress（端口属结构变更）；
3. 落盘：重启前将新配置写入配置文件 / ConfigMap，或在重启后重新 import（此时不再报
   `requires_restart`）；
4. watcher 检测到结构差异同样只告警、不应用（保持旧配置），勿依赖热重载生效。

### 管理端删除会话 = 全站踢出

共享会话（同一 `session_profile`，如 `cookie_domain: .example.com`）下，
`DELETE /admin/api/sessions/:id`（管理台「删除会话」）清掉该会话的**全部站点 token**，等效于
**该用户在所有站点被踢出**。管理台按钮已提示此语义；运维执行前确认影响范围（用户下次访问任一站点
需重新登录）。`GET /admin/api/sites` 返回站点清单（`name` / `port` / `public_base_url` /
`default_provider` / `providers` / `session_profile` / `logout_scope` / `legacy`）；
`GET /admin/api/sessions` 的每项现携带 `sites` / `providers`（当前会话实际持有 token 的站点与
provider 集合）。

---

## 配置相关应急操作

| 操作 | 步骤 |
| --- | --- |
| 回滚最近一次管理端配置 | 用变更前的 `config/export` 文件重新 import（`***` 哨兵会保留现网密钥） |
| 强制放弃运行时覆盖 | 停止实例 → 移除/备份 `runtime.yaml` → 重启（回落到 base/env 配置） |
| 密钥轮换 | 走 `docs/deployment.md` 的「迁移预警」：与 Session 迁移合并窗口执行，重启生效，全员重登 |
| 恢复单实例形态 | 临时置 `provider.*=memory` **仅限已声明风险的单副本环境**；prod 防呆会拒绝，需同时改环境标记（不建议） |
| 导入报 `requires_restart` | 变更属启动物化字段（如 `sites[].port`、profile cookie 策略）；见「多站点」节，走滚动发布 / 重启，不落盘 |
| 回滚多站点配置 | 删除 `sites` / `session_profiles` 并还原 `session.cookie_name`；Domain cookie 忽略后自然过期，手动下发 `Max-Age=0` 可立即清理（详见 [deployment.md](deployment.md) §7.4） |
