//! OTel（OTLP）导出契约测试。
//!
//! 用**进程内 OTLP/gRPC collector**（tonic；与生产导出路径完全一致）验证：
//! 1. 未配置 `telemetry.otlp_endpoint` → 完全禁用（`None`，不产生任何出站）；
//! 2. 启用后 span 经 OTLP/gRPC 真实导出：collector 收到导出请求，且
//!    resource `service.name` 正确、span 名与属性符合预期；
//! 3. **跨服务衔接**：入站 `traceparent` 作为远程父上下文 → 导出 span 的
//!    `trace_id` 延续入站 trace、`parent_span_id` == 入站 span_id；
//! 4. 传播语义：[`bff::telemetry::current_span_traceparent`] 复用入站 trace_id、
//!    生成新 span_id，并如实携带采样位（01/00）。
//! 5. 配置校验：非法 endpoint / 越界采样率被 `validate()` 拒绝。
//! 6. 生产接线端到端：真实 HTTP 请求的响应 `traceparent` 与 collector 中导出的
//!    `http.request` span 完全一致（span_id 相同、parent 指向入站）。
//!
//! 运行时说明：所有用例使用 `flavor = "multi_thread"` 并显式 `shutdown_async()`。
//! SDK 的 `BatchSpanProcessor` 在关停（含 provider drop）时用 `futures_executor::block_on`
//! 等待后台任务，在 current_thread 运行时上会死锁——与生产 `#[tokio::main]`（multi_thread）
//! 保持一致并使用异步安全关停（见 `TelemetryHandle::shutdown_async`）。

mod common;

use bff::config::TelemetryConfig;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tracing_subscriber::layer::SubscriberExt;

const PARENT_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT_SPAN: &str = "00f067aa0ba902b7";

// ============================================================
// 进程内 OTLP/gRPC collector
// ============================================================

struct TestCollector {
    tx: mpsc::UnboundedSender<ExportTraceServiceRequest>,
}

#[tonic::async_trait]
impl TraceService for TestCollector {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let _ = self.tx.send(request.into_inner());
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

async fn spawn_collector() -> (String, mpsc::UnboundedReceiver<ExportTraceServiceRequest>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(TraceServiceServer::new(TestCollector { tx }))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    (format!("http://{addr}"), rx)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 在局部 subscriber（含 OTel 导出层）中执行闭包；避免全局 subscriber 冲突。
fn with_otel_subscriber<T>(handle: &bff::telemetry::TelemetryHandle, f: impl FnOnce() -> T) -> T {
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(handle.tracer()));
    tracing::subscriber::with_default(subscriber, f)
}

// ============================================================
// 1. 默认禁用 / 非法配置
// ============================================================

#[tokio::test(flavor = "multi_thread")]
async fn telemetry_disabled_by_default() {
    let cfg = TelemetryConfig::default();
    assert!(
        bff::telemetry::init(&cfg)
            .expect("禁用路径不应报错")
            .is_none(),
        "otlp_endpoint 为空时必须完全禁用导出"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn telemetry_init_rejects_invalid_endpoint() {
    let cfg = TelemetryConfig {
        otlp_endpoint: Some("not-a-url".into()),
        ..Default::default()
    };
    assert!(
        bff::telemetry::init(&cfg).is_err(),
        "非法 endpoint 必须初始化失败（fail-fast，而非静默禁用）"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn telemetry_config_validation() {
    let mut cfg = common::base_config();
    cfg.server.business_port = 8080;
    cfg.server.admin_port = 8443;

    // 合法（启用导出）
    cfg.telemetry = TelemetryConfig {
        otlp_endpoint: Some("http://otel-collector:4317".into()),
        ..Default::default()
    };
    cfg.validate().expect("合法 telemetry 配置应通过校验");

    // 非法采样率
    cfg.telemetry.sample_ratio = 1.5;
    assert!(cfg.validate().is_err(), "sample_ratio > 1 应被拒绝");
    cfg.telemetry.sample_ratio = -0.1;
    assert!(cfg.validate().is_err(), "sample_ratio < 0 应被拒绝");

    // 非法 scheme
    cfg.telemetry.sample_ratio = 1.0;
    cfg.telemetry.otlp_endpoint = Some("ftp://otel:4317".into());
    assert!(cfg.validate().is_err(), "非 http(s) endpoint 应被拒绝");
}

// ============================================================
// 2. OTLP/gRPC 端到端导出 + 父链路衔接
// ============================================================

#[tokio::test(flavor = "multi_thread")]
async fn otlp_export_roundtrip_with_parent_linkage() {
    let (endpoint, mut rx) = spawn_collector().await;
    let cfg = TelemetryConfig {
        otlp_endpoint: Some(endpoint),
        service_name: "bff-test".into(),
        sample_ratio: 1.0,
    };
    let handle = bff::telemetry::init(&cfg)
        .expect("初始化应成功")
        .expect("应启用导出");

    with_otel_subscriber(&handle, || {
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        let parent = bff::telemetry::context_from_traceparent(&format!(
            "00-{PARENT_TRACE}-{PARENT_SPAN}-01"
        ))
        .expect("构造父上下文");
        let span = tracing::info_span!("test.span", "http.method" = "GET");
        span.set_parent(parent).expect("设置父上下文");
        let _g = span.enter();
        tracing::info!("inside span");
    });
    // flush + 关停（BatchSpanProcessor 立即导出队列中的 span）；异步安全关停见文件头说明
    handle.shutdown_async().await;

    let request = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("等待 collector 接收导出超时")
        .expect("collector 未收到任何导出请求");

    // resource 属性：service.name 必须为配置值（供 collector 侧区分服务）
    let resource_spans = request.resource_spans;
    assert_eq!(resource_spans.len(), 1, "应导出 1 个 resource 分组");
    let rs = &resource_spans[0];
    let resource = rs.resource.as_ref().expect("resource 不应为空");
    let attrs: HashMap<&str, &str> = resource
        .attributes
        .iter()
        .filter_map(|kv| {
            let value = kv.value.as_ref()?.value.as_ref()?;
            match value {
                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => {
                    Some((kv.key.as_str(), s.as_str()))
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(attrs.get("service.name"), Some(&"bff-test"));
    assert!(attrs.contains_key("service.version"));

    // span：名称、trace 衔接（trace_id 延续、parent_span_id == 入站 span_id）
    assert_eq!(rs.scope_spans.len(), 1);
    let span = rs.scope_spans[0]
        .spans
        .iter()
        .find(|s| s.name == "test.span")
        .expect("应导出 test.span");
    assert_eq!(
        to_hex(&span.trace_id),
        PARENT_TRACE,
        "trace_id 必须延续入站"
    );
    assert_eq!(
        to_hex(&span.parent_span_id),
        PARENT_SPAN,
        "parent_span_id 必须指向入站 span（跨服务链路衔接）"
    );
    assert_ne!(
        to_hex(&span.span_id),
        PARENT_SPAN,
        "本跳 span_id 应为新生成"
    );

    // 属性透传：http.method 出现在导出 span 上
    let method = span.attributes.iter().find(|kv| kv.key == "http.method");
    let method_value = method
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.value.as_ref());
    assert!(
        matches!(
            method_value,
            Some(opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s)) if s == "GET"
        ),
        "http.method 属性应随 span 导出"
    );
}

// ============================================================
// 3. 传播语义：trace 延续与采样位如实携带
// ============================================================

/// 生产接线路径：`BffMakeSpan` 从入站请求头提取 traceparent 并作为远程父上下文。
#[tokio::test(flavor = "multi_thread")]
async fn bff_make_span_links_remote_parent_from_header() {
    let cfg = TelemetryConfig {
        otlp_endpoint: Some("http://127.0.0.1:9".into()),
        service_name: "bff-test".into(),
        sample_ratio: 1.0,
    };
    let handle = bff::telemetry::init(&cfg).expect("初始化应成功").unwrap();

    let traceparent = with_otel_subscriber(&handle, || {
        use tower_http::trace::MakeSpan;
        let req = axum::http::Request::builder()
            .uri("http://bff.local/api/users?x=1")
            .header("traceparent", format!("00-{PARENT_TRACE}-{PARENT_SPAN}-01"))
            .body(())
            .unwrap();
        let span = bff::middleware::trace_context::BffMakeSpan.make_span(&req);
        let _g = span.enter();
        bff::telemetry::current_span_traceparent()
    })
    .expect("启用导出层后应生成 traceparent");

    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts[1], PARENT_TRACE, "make_span 应延续入站 trace");
    assert_ne!(parts[2], PARENT_SPAN, "本跳 span_id 应为新生成");
    assert_eq!(parts[3], "01");

    handle.shutdown_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn traceparent_continues_parent_trace_and_respects_sampling() {
    // endpoint 指向关闭端口：本用例只验证传播语义，不产生导出等待
    let cfg = TelemetryConfig {
        otlp_endpoint: Some("http://127.0.0.1:9".into()),
        service_name: "bff-test".into(),
        sample_ratio: 1.0,
    };
    let handle = bff::telemetry::init(&cfg).expect("初始化应成功").unwrap();

    for (flags, expect_sampled) in [("01", "01"), ("00", "00")] {
        let parent = format!("00-{PARENT_TRACE}-{PARENT_SPAN}-{flags}");
        let traceparent = with_otel_subscriber(&handle, || {
            use tracing_opentelemetry::OpenTelemetrySpanExt;
            let span = tracing::info_span!("propagation.test");
            span.set_parent(
                bff::telemetry::context_from_traceparent(&parent).expect("构造父上下文"),
            )
            .expect("设置父上下文");
            let _g = span.enter();
            bff::telemetry::current_span_traceparent()
        })
        .expect("启用导出层后应生成 traceparent");

        let parts: Vec<&str> = traceparent.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[1], PARENT_TRACE, "trace_id 必须延续（flags={flags}）");
        assert_ne!(parts[2], PARENT_SPAN, "本跳 span_id 应为新生成");
        assert_eq!(
            parts[3], expect_sampled,
            "采样位必须如实透传（入站 {flags}）"
        );
    }

    handle.shutdown_async().await;
}

// ============================================================
// 4. 生产接线端到端：请求 → traceparent → 导出 span 一致
// ============================================================

/// 全局 subscriber 只允许安装一次（本二进制内）。
fn install_global_otel_subscriber(handle: &bff::telemetry::TelemetryHandle) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(handle.tracer()));
        // 已安装（例如重复调用）不影响用例
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// 真实 HTTP 请求路径：
/// - 响应 `traceparent` 的 trace/span 与 collector 中导出的 `http.request` span 完全一致；
/// - 导出 span 的 parent 指向入站 `traceparent` 的 span（跨服务链路衔接）；
/// - `http.method` / `http.target` / `http.status_code` 属性符合 OTel semconv。
#[tokio::test(flavor = "multi_thread")]
async fn http_request_span_exported_and_linked() {
    let (endpoint, mut rx) = spawn_collector().await;
    let cfg = TelemetryConfig {
        otlp_endpoint: Some(endpoint),
        service_name: "bff-e2e".into(),
        sample_ratio: 1.0,
    };
    let handle = bff::telemetry::init(&cfg)
        .expect("初始化应成功")
        .expect("应启用导出");
    install_global_otel_subscriber(&handle);

    // 业务路由（全局 subscriber 已安装 → TraceLayer span 会被导出）
    let app_cfg = common::base_config();
    let base = common::spawn_business(common::make_state(app_cfg)).await;
    let client = common::test_client();
    let inbound = format!("00-{PARENT_TRACE}-{PARENT_SPAN}-01");
    let resp = client
        .get(format!("{base}/live"))
        .header("traceparent", &inbound)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 200);
    let resp_tp = resp
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .expect("响应应带 traceparent")
        .to_string();
    let resp_parts: Vec<&str> = resp_tp.split('-').collect();
    assert_eq!(resp_parts[1], PARENT_TRACE, "响应 trace 应延续入站");
    assert_eq!(resp_parts[3], "01");

    // 关停 flush 后从 collector 取回 span
    handle.shutdown_async().await;
    let request = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("等待导出超时")
        .expect("collector 未收到导出请求");

    let req_span = request
        .resource_spans
        .iter()
        .flat_map(|rs| rs.scope_spans.iter())
        .flat_map(|ss| ss.spans.iter())
        .find(|s| s.name == "http.request" && to_hex(&s.trace_id) == PARENT_TRACE)
        .expect("应导出本次请求的 http.request span");

    assert_eq!(
        to_hex(&req_span.parent_span_id),
        PARENT_SPAN,
        "导出 span 的 parent 应指向入站 span"
    );
    assert_eq!(
        to_hex(&req_span.span_id),
        resp_parts[2],
        "响应 traceparent 的 span-id 必须等于导出 span 的 span_id"
    );

    let attr = |key: &str| -> Option<String> {
        req_span
            .attributes
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| kv.value.as_ref())
            .and_then(|v| v.value.as_ref())
            .map(|v| match v {
                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => {
                    s.clone()
                }
                opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => {
                    i.to_string()
                }
                other => format!("{other:?}"),
            })
    };
    assert_eq!(attr("http.method").as_deref(), Some("GET"));
    assert_eq!(attr("http.target").as_deref(), Some("/live"));
    assert_eq!(attr("http.status_code").as_deref(), Some("200"));
    // §10/§14：span 属性携带站点名（legacy 配置合成 `default` 站点）
    assert_eq!(
        attr("bff.site").as_deref(),
        Some("default"),
        "http.request span 应带 bff.site=default"
    );
}
