//! Task 11：per-site router 运行时集成测试（§6.1 / §9 / §10）。
//!
//! 每个站点一个 router：SPA 目录、站点过滤路由（§5.3）与 metrics site 标签均按站点生效；
//! legacy 入口 `build_business_router` 拒绝显式多站点配置。

mod common;

use bff::config::SpaConfig;
use std::sync::{Arc, Mutex};

/// 站点级 SPA 目录 + 站点过滤路由隔离（§9）。
///
/// - `/index.html` 按站点 `spa.dir` 返回各自目录内容；
/// - 站点限定路由只在本站点命中，跨站请求在 SPA fallback 前被 `/api` 前缀拦截 → 404。
#[tokio::test]
async fn per_site_spa_and_route_isolation() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    cfg.sites[0].spa = Some(SpaConfig {
        dir: common::make_spa_dir("a"),
    });
    cfg.sites[1].spa = Some(SpaConfig {
        dir: common::make_spa_dir("b"),
    });
    // 两个目录的默认内容一致（make_spa_dir 生成相同 index.html），
    // 覆盖为站点专属内容以验证目录隔离。
    std::fs::write(
        format!("{}/index.html", cfg.sites[0].spa.as_ref().unwrap().dir),
        "<h1>site-a</h1>",
    )
    .unwrap();
    std::fs::write(
        format!("{}/index.html", cfg.sites[1].spa.as_ref().unwrap().dir),
        "<h1>site-b</h1>",
    )
    .unwrap();
    cfg.routes.push(common::route_with_site("/api/a", "app1"));
    cfg.routes.push(common::route_with_site("/api/b", "app2"));
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;

    let client = common::test_client();

    // 1. SPA：/index.html 按站点目录返回
    let resp = client
        .get(format!("{}/index.html", a))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("site-a"));
    let resp = client
        .get(format!("{}/index.html", b))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("site-b"));

    // 2. 站点过滤路由：本站点请求命中
    let resp = client.get(format!("{}/api/a", a)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client.get(format!("{}/api/b", b)).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 3. 隔离：A 的 /api/b 与 B 的 /api/a → 404（SPA fallback 前被 /api 前缀拦截）
    let resp = client.get(format!("{}/api/b", a)).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    let resp = client.get(format!("{}/api/a", b)).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

/// 显式多站点配置必须用 `build_site_router`；legacy 入口拒绝并报错（§6.1）。
#[tokio::test]
async fn business_router_rejects_explicit_multisite() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));
    assert!(bff::server::business::build_business_router(state).is_err());
}

/// 指标捕获 recorder（`register_*` 时记录 key 名 + 标签）。
///
/// `metrics::counter!`/`histogram!` 宏每次调用都会以当前标签集构造 key 并触发
/// `register_*`，因此按「(指标名, 标签集)」维度捕获即可验证 site 标签与探针排除。
/// 捕获到的指标样本：(指标名, 标签集)。
type SeenMetric = (String, Vec<(String, String)>);

struct MetricLabels {
    seen: Arc<Mutex<Vec<SeenMetric>>>,
}

impl MetricLabels {
    fn capture(&self, key: &metrics::Key) {
        let name = key.name().to_string();
        let labels = key
            .labels()
            .map(|l| (l.key().to_string(), l.value().to_string()))
            .collect();
        self.seen.lock().unwrap().push((name, labels));
    }
}

impl metrics::Recorder for MetricLabels {
    fn describe_counter(
        &self,
        _k: metrics::KeyName,
        _u: Option<metrics::Unit>,
        _d: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _k: metrics::KeyName,
        _u: Option<metrics::Unit>,
        _d: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _k: metrics::KeyName,
        _u: Option<metrics::Unit>,
        _d: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _m: &metrics::Metadata<'_>) -> metrics::Counter {
        self.capture(key);
        metrics::Counter::noop()
    }
    fn register_gauge(&self, _k: &metrics::Key, _m: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }
    fn register_histogram(
        &self,
        key: &metrics::Key,
        _m: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        self.capture(key);
        metrics::Histogram::noop()
    }
}

/// §10：业务请求指标带 `site` 标签；`/live`、`/ready` 探针不进入业务指标。
///
/// 线程本地 recorder（`metrics::set_default_local_recorder`）：`#[tokio::test]`
/// 为 current_thread 运行时，spawn 出的 server 任务与测试共享同一线程，指标调用可见。
#[tokio::test]
async fn metrics_carry_site_label_and_probes_are_excluded() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = MetricLabels { seen: seen.clone() };
    let _guard = metrics::set_default_local_recorder(&recorder);

    let client = common::test_client();
    for url in [
        format!("{}/index.html", a),
        format!("{}/index.html", b),
        format!("{}/live", a),
        format!("{}/live", b),
        format!("{}/ready", a),
    ] {
        let _ = client.get(&url).send().await;
    }
    drop(_guard);

    let seen = seen.lock().unwrap();
    let label = |name: &str| {
        seen.iter()
            .filter(|(metric, _)| metric == name)
            .map(|(_, labels)| labels)
            .collect::<Vec<_>>()
    };

    for metric in [
        "bff_http_requests_total",
        "bff_http_request_duration_seconds",
    ] {
        let sets = label(metric);
        // 业务请求：site 标签 = 各自站点名
        assert!(
            sets.iter()
                .any(|l| l.contains(&("site".into(), "app1".into()))),
            "{metric} 应携带 site=app1 标签"
        );
        assert!(
            sets.iter()
                .any(|l| l.contains(&("site".into(), "app2".into()))),
            "{metric} 应携带 site=app2 标签"
        );
        // 探针请求不产生业务指标
        assert!(
            sets.iter().all(|l| {
                !l.contains(&("path".into(), "/live".into()))
                    && !l.contains(&("path".into(), "/ready".into()))
            }),
            "{metric} 不得包含 /live、/ready 探针请求"
        );
    }
}
