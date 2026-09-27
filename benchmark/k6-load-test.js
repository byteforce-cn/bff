// ============================================================
// BFF 基准负载测试 (k6)
// ============================================================
// 运行方式:
//   k6 run k6-load-test.js
//   k6 run --vus 50 --duration 60s k6-load-test.js
//   k6 run --env BASE_URL=http://localhost:8080 k6-load-test.js
//
// 认证路径压测（代理/编排需要会话）：
//   k6 run --env COOKIE="BFF_SESSION=<session-id>" --env SCENARIO=baseline k6-load-test.js
//   COOKIE 获取方式见 benchmark/README.md「认证路径」一节。
//
// 无本机 k6 时用 Docker（宿主机网络，直连 127.0.0.1 与 --network host 一致）：
//   docker run --rm -i --network host -v "$PWD":/bench -w /bench \
//     grafana/k6 run --env SCENARIO=baseline k6-load-test.js
//
// 场景说明:
//   smoke      — 冒烟测试：1 VU，验证所有端点可用
//   baseline   — 基线测试：阶梯式加压，找到单实例 QPS 上限
//   capacity   — 容量测试：200→800 VU，挤压单实例容量拐点（比 baseline 更重）
//   stress     — 压力测试：保持高负载，观察降级行为
//   endurance  — 耐久测试：长时间中等负载，检测内存泄漏
// ============================================================

import { check, sleep, group } from "k6";
import { Counter, Rate, Trend } from "k6/metrics";
import http from "k6/http";

// ---- 自定义指标 ----
const pipelineDuration = new Trend("bff_pipeline_duration_ms");
const proxyDuration = new Trend("bff_proxy_duration_ms");
const errorsByEndpoint = new Counter("bff_errors_by_endpoint");
const errorRate = new Rate("bff_error_rate");

// ---- 配置 ----
const BASE_URL = __ENV.BASE_URL || "http://127.0.0.1:8080";
const SCENARIO = __ENV.SCENARIO || "smoke"; // smoke | baseline | stress | endurance
const RESULTS_DIR = __ENV.RESULTS_DIR || "./results";
// 认证会话 Cookie（形如 "BFF_SESSION=xxx"）；提供后所有请求携带（代理/编排认证路径）
const COOKIE = __ENV.COOKIE || "";
// 思考时间：smoke/endurance 保持贴近真实用户（0.5s）；baseline/stress 最小化以逼近 QPS 上限
const THINK_TIME = SCENARIO === "smoke" || SCENARIO === "endurance" ? 0.5 : 0.05;

// ---- 场景配置 ----
export const options = {
  scenarios: SCENARIO === "smoke" ? {
    smoke: {
      executor: "constant-vus",
      vus: 1,
      duration: "30s",
    },
  } : SCENARIO === "baseline" ? {
    baseline: {
      executor: "ramping-vus",
      startVUs: 1,
      stages: [
        { duration: "30s", target: 10 },
        { duration: "30s", target: 50 },
        { duration: "30s", target: 100 },
        { duration: "30s", target: 200 },
        { duration: "30s", target: 0 },
      ],
      gracefulRampDown: "10s",
    },
  } : SCENARIO === "capacity" ? {
    capacity: {
      executor: "ramping-vus",
      startVUs: 100,
      stages: [
        { duration: "45s", target: 300 },
        { duration: "45s", target: 500 },
        { duration: "45s", target: 800 },
        { duration: "15s", target: 800 },
      ],
      gracefulRampDown: "15s",
    },
  } : SCENARIO === "stress" ? {
    stress: {
      executor: "ramping-vus",
      startVUs: 0,
      stages: [
        { duration: "30s", target: 100 },
        { duration: "2m", target: 100 },
        { duration: "30s", target: 200 },
        { duration: "2m", target: 200 },
        { duration: "30s", target: 0 },
      ],
      gracefulRampDown: "30s",
    },
  } : /* endurance */ {
    endurance: {
      executor: "constant-vus",
      vus: 50,
      duration: "10m",
    },
  },

  thresholds: SCENARIO === "smoke" ? {
    http_req_duration: ["p(95) < 500"],   // smoke: 所有端点 p95 < 500ms
    http_req_failed: ["rate < 0.01"],      // smoke: 错误率 < 1%（无认证端点应全绿）
  } : {
    http_req_duration: ["p(95) < 2000"], // p95 < 2s
    http_req_failed: ["rate < 0.01"], // 错误率 < 1%
    // 按端点声明阈值 → k6 汇总数据中产出 `http_req_duration{endpoint:...}` 子指标，
    // 基线报告需要分端点的 p95（详见 benchmark/README.md 基线数值表）。
    "http_req_duration{endpoint:liveness}": ["p(95) < 1000"],
    "http_req_duration{endpoint:readiness}": ["p(95) < 2000"],
    "http_req_duration{endpoint:echo}": ["p(95) < 2000"],
    "http_req_duration{endpoint:health}": ["p(95) < 1000"],
    "http_req_duration{endpoint:users}": ["p(95) < 2000"],
    "http_req_duration{endpoint:orders}": ["p(95) < 2000"],
  },

  summaryTrendStats: ["avg", "min", "med", "p(90)", "p(95)", "p(99)", "max"],
};

// ---- 工具函数 ----
function endpoint(name, method, path, body, params) {
  const url = `${BASE_URL}${path}`;
  // 端点标签用于子指标拆分；COOKIE 提供时统一附加（认证路径）
  const opts = Object.assign({}, params || {});
  opts.tags = Object.assign({}, opts.tags || {}, { endpoint: name });
  if (COOKIE) {
    opts.headers = Object.assign({}, opts.headers || {}, { Cookie: COOKIE });
  }
  const start = Date.now();
  let resp;
  if (method === "POST") {
    resp = http.post(url, body, opts);
  } else if (method === "PUT") {
    resp = http.put(url, body, opts);
  } else if (method === "DELETE") {
    resp = http.del(url, null, opts);
  } else {
    resp = http.get(url, opts);
  }
  const elapsed = Date.now() - start;

  const tags = { endpoint: name };
  const ok = check(resp, {
    [`${name}: status 2xx`]: (r) => r.status >= 200 && r.status < 300,
  }, tags);
  if (!ok) {
    errorsByEndpoint.add(1, tags);
    errorRate.add(1);
  } else {
    errorRate.add(0);
  }
  return { resp, elapsed };
}

// ---- 测试函数 ----
function testLiveness() {
  return endpoint("liveness", "GET", "/live");
}

function testReadiness() {
  return endpoint("readiness", "GET", "/ready");
}

function testEchoPipeline() {
  const { elapsed } = endpoint("echo", "GET", "/api/echo");
  pipelineDuration.add(elapsed);
}

function testHealthStatic() {
  return endpoint("health", "GET", "/api/health");
}

function testUsersProxy() {
  const { elapsed } = endpoint("users", "GET", "/api/users");
  proxyDuration.add(elapsed);
}

function testOrdersProxy() {
  const { elapsed } = endpoint("orders", "GET", "/api/orders");
  proxyDuration.add(elapsed);
}

// ---- VU 生命周期 ----
export default function () {
  group("health check", function () {
    testLiveness();
    sleep(THINK_TIME);
  });

  group("pipeline echo", function () {
    testEchoPipeline();
    testHealthStatic();
    sleep(THINK_TIME);
  });

  // 代理端点需要认证 session，仅在非 smoke 场景测试
  // smoke 只验证核心路径（无需外部依赖）
  if (SCENARIO !== "smoke") {
    group("proxy routes", function () {
      testUsersProxy();
      testOrdersProxy();
      sleep(THINK_TIME);
    });
  }

  group("readiness", function () {
    testReadiness();
    sleep(THINK_TIME);
  });
}

// ---- 报告摘要 ----
export function handleSummary(data) {
  const now = new Date();
  const ts = now.toISOString().replace(/[:.]/g, "-");
  const scenario = SCENARIO;
  const vusMax = data.metrics.vus_max?.values?.max || "N/A";

  // 提取关键指标
  const httpReqDuration = data.metrics.http_req_duration?.values || {};
  const httpReqFailed = data.metrics.http_req_failed?.values || {};
  const iterations = data.metrics.iterations?.values?.count || 0;
  const totalReqs = data.metrics.http_reqs?.values?.count || 0;
  const dataReceived = data.metrics.data_received?.values?.count || 0;
  const dataSent = data.metrics.data_sent?.values?.count || 0;

  const report = {
    scenario,
    base_url: BASE_URL,
    duration_seconds: (data.state.testRunDurationMs / 1000).toFixed(0),
    max_vus: vusMax,
    total_requests: totalReqs,
    total_iterations: iterations,
    data_received_mb: (dataReceived / 1024 / 1024).toFixed(2),
    data_sent_mb: (dataSent / 1024 / 1024).toFixed(2),
    http_req_duration_ms: {
      avg: httpReqDuration.avg?.toFixed(2),
      p50: httpReqDuration.med?.toFixed(2),
      p90: httpReqDuration["p(90)"]?.toFixed(2),
      p95: httpReqDuration["p(95)"]?.toFixed(2),
      p99: httpReqDuration["p(99)"]?.toFixed(2),
      max: httpReqDuration.max?.toFixed(2),
    },
    http_req_failed: httpReqFailed.passes !== undefined
      ? `${(httpReqFailed.passes / (httpReqFailed.passes + httpReqFailed.fails) * 100).toFixed(2)}%`
      : "N/A",
    qps: (totalReqs / (data.state.testRunDurationMs / 1000)).toFixed(1),
  };

  // 打印控制台摘要
  console.log("\n" + "=".repeat(60));
  console.log("BFF Benchmark Summary");
  console.log("=".repeat(60));
  console.log(`Scenario:      ${report.scenario}`);
  console.log(`Base URL:      ${report.base_url}`);
  console.log(`Duration:      ${report.duration_seconds}s`);
  console.log(`Max VUs:       ${report.max_vus}`);
  console.log(`Total Reqs:    ${report.total_requests}`);
  console.log(`QPS:           ${report.qps}`);
  console.log(`Error Rate:    ${report.http_req_failed}`);
  console.log("-".repeat(60));
  console.log("Latency (ms):");
  console.log(`  avg:  ${report.http_req_duration_ms.avg}`);
  console.log(`  p50:  ${report.http_req_duration_ms.p50}`);
  console.log(`  p90:  ${report.http_req_duration_ms.p90}`);
  console.log(`  p95:  ${report.http_req_duration_ms.p95}`);
  console.log(`  p99:  ${report.http_req_duration_ms.p99}`);
  console.log(`  max:  ${report.http_req_duration_ms.max}`);
  console.log("=".repeat(60));

  return {
    "stdout": `\n${JSON.stringify(report, null, 2)}\n`,
    [`${RESULTS_DIR}/benchmark-${scenario}-${ts}.json`]: JSON.stringify(data, null, 2),
  };
}
