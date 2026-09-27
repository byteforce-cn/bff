//! 轻量熔断器：按 key（建议路由 path）维度统计失败，超阈值后短路一段时间。
//!
//! 语义（R3/R15）：
//! - **滚动窗口失败计数**：`failure_window` 内失败次数达到阈值即熔断（成功不清零窗口，
//!   间歇性故障同样会触发；窗口外的失败自动衰减）；
//! - **半开单探针**：冷却结束后仅放行一个探针请求，其余请求继续 503；
//!   探针成功 → 闭合，失败 → 重新熔断；
//! - **路由级阈值**：`allow(key, Some(n))` 为该 key 固化阈值，`Some(0)` 表示不熔断。
//!
//! 使用 tokio::sync::Mutex 避免阻塞异步运行时。
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug)]
struct Breaker {
    state: BreakerState,
    /// 滚动窗口内的失败时刻（R15：窗口内累计，而非“连续失败”——间歇性故障同样会触发）
    failures: std::collections::VecDeque<Instant>,
    /// 本熔断键的阈值（0 = 该路由不熔断；由首次 allow 时的路由级配置或全局默认决定）
    threshold: u32,
    opened_at: Option<Instant>,
    /// R3：半开状态只放行单探针，防恢复瞬间流量全线涌入
    probe_in_flight: bool,
    probe_started_at: Option<Instant>,
}

impl Default for Breaker {
    fn default() -> Self {
        Self {
            state: BreakerState::Closed,
            failures: std::collections::VecDeque::new(),
            threshold: 0,
            opened_at: None,
            probe_in_flight: false,
            probe_started_at: None,
        }
    }
}

#[derive(Clone)]
pub struct CircuitBreakerRegistry {
    inner: Arc<Mutex<HashMap<String, Breaker>>>,
    default_threshold: u32,
    failure_window: Duration,
    open_duration: Duration,
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            default_threshold: 5,
            failure_window: Duration::from_secs(60),
            open_duration: Duration::from_secs(30),
        }
    }
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用自定义参数创建熔断器注册表。
    pub fn new_with_config(
        failure_threshold: u32,
        failure_window: Duration,
        open_duration: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            default_threshold: failure_threshold,
            failure_window,
            open_duration,
        }
    }

    /// 调用前检查：Open 状态且未到冷却期则拒绝；HalfOpen 仅放行单探针。
    ///
    /// `threshold_override` 为路由级阈值（`RouteTypeConfig::circuit_breaker_threshold`）：
    /// - `Some(0)` → 该路由不熔断（永远放行）；
    /// - `Some(n)` → 以 n 作为该 key 的阈值（首次建立时固化）；
    /// - `None` → 使用全局默认阈值。
    ///
    /// key 建议使用“路由 path”（而非 upstream），这样路由级阈值才有确定语义。
    pub async fn allow(&self, key: &str, threshold_override: Option<u32>) -> bool {
        let mut map = self.inner.lock().await;
        let open_dur = self.open_duration;
        let default_threshold = self.default_threshold;
        let b = map.entry(key.to_string()).or_insert_with(|| Breaker {
            threshold: threshold_override.unwrap_or(default_threshold),
            ..Default::default()
        });
        if b.threshold == 0 {
            return true;
        }
        match b.state {
            BreakerState::Closed => true,
            BreakerState::HalfOpen => {
                // 单探针：已有探针在途则拒绝；探针超时（大于冷却期视为死亡）则允许新探针
                let probe_stale = b
                    .probe_started_at
                    .map(|t| t.elapsed() >= open_dur)
                    .unwrap_or(true);
                if b.probe_in_flight && !probe_stale {
                    false
                } else {
                    b.probe_in_flight = true;
                    b.probe_started_at = Some(Instant::now());
                    metrics::counter!("bff_circuit_breaker_probe_total", "upstream" => key.to_string())
                        .increment(1);
                    true
                }
            }
            BreakerState::Open => {
                if b.opened_at
                    .map(|t| t.elapsed() >= open_dur)
                    .unwrap_or(false)
                {
                    b.state = BreakerState::HalfOpen;
                    b.probe_in_flight = true;
                    b.probe_started_at = Some(Instant::now());
                    metrics::counter!("bff_circuit_breaker_probe_total", "upstream" => key.to_string())
                        .increment(1);
                    true
                } else {
                    false
                }
            }
        }
    }

    pub async fn record_success(&self, key: &str) {
        let mut map = self.inner.lock().await;
        let Some(b) = map.get_mut(key) else {
            return;
        };
        if b.threshold == 0 {
            return;
        }
        // R15：Closed 状态下成功**不清零**滚动窗口（否则退化为“连续失败”语义，
        // 间歇性故障永不触发）；仅担当 Open/HalfOpen → Closed 的恢复信号。
        if b.state == BreakerState::Closed {
            return;
        }
        b.state = BreakerState::Closed;
        b.failures.clear();
        b.opened_at = None;
        b.probe_in_flight = false;
        b.probe_started_at = None;
    }

    pub async fn record_failure(&self, key: &str) {
        let mut map = self.inner.lock().await;
        let window = self.failure_window;
        let default_threshold = self.default_threshold;
        let b = map.entry(key.to_string()).or_insert_with(|| Breaker {
            threshold: default_threshold,
            ..Default::default()
        });
        if b.threshold == 0 {
            return;
        }
        let now = Instant::now();
        // HalfOpen 下探针失败 → 立即重新熔断
        if b.state == BreakerState::HalfOpen {
            b.state = BreakerState::Open;
            b.opened_at = Some(now);
            b.failures.clear();
            b.probe_in_flight = false;
            b.probe_started_at = None;
            metrics::counter!("bff_circuit_breaker_open_total", "upstream" => key.to_string())
                .increment(1);
            return;
        }
        // 滚动窗口：过期失败自动衰减
        while let Some(front) = b.failures.front() {
            if now.duration_since(*front) > window {
                b.failures.pop_front();
            } else {
                break;
            }
        }
        b.failures.push_back(now);
        if b.failures.len() as u32 >= b.threshold {
            b.state = BreakerState::Open;
            b.opened_at = Some(now);
            b.failures.clear();
            b.probe_in_flight = false;
            b.probe_started_at = None;
            metrics::counter!("bff_circuit_breaker_open_total", "upstream" => key.to_string())
                .increment(1);
        }
    }

    #[cfg(test)]
    pub async fn state_of(&self, upstream: &str) -> BreakerState {
        self.inner
            .lock()
            .await
            .get(upstream)
            .map(|b| b.state)
            .unwrap_or(BreakerState::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_after_threshold() {
        let reg = CircuitBreakerRegistry::new();
        let threshold = 5; // default failure_threshold
                           // 预先建立 key（固化阈值），避免依赖 allow 的副作用
        assert!(reg.allow("up", None).await);
        for _ in 0..threshold {
            reg.record_failure("up").await;
        }
        assert_eq!(reg.state_of("up").await, BreakerState::Open);
        assert!(!reg.allow("up", None).await);
        reg.record_success("up").await;
        assert_eq!(reg.state_of("up").await, BreakerState::Closed);
    }

    #[tokio::test]
    async fn intermittent_failures_accumulate_in_window() {
        let reg = CircuitBreakerRegistry::new();
        assert!(reg.allow("up", None).await);
        // 失败-成功交替：滚动窗口下失败仍然累计（旧语义“连续失败”永不触发）；
        // 成功不清零窗口，但计入第 5 次失败后立即熔断。
        for _ in 0..2 {
            reg.record_failure("up").await;
            reg.record_success("up").await;
        }
        for _ in 0..3 {
            reg.record_failure("up").await;
        }
        assert_eq!(reg.state_of("up").await, BreakerState::Open);
        // 恢复信号（成功）仍可闭合
        reg.record_success("up").await;
        assert_eq!(reg.state_of("up").await, BreakerState::Closed);
    }

    #[tokio::test]
    async fn half_open_single_probe() {
        let reg = CircuitBreakerRegistry::new_with_config(
            2,
            Duration::from_secs(60),
            Duration::from_millis(100),
        );
        assert!(reg.allow("up", None).await);
        reg.record_failure("up").await;
        reg.record_failure("up").await;
        assert_eq!(reg.state_of("up").await, BreakerState::Open);
        // 冷却期内拒绝
        assert!(!reg.allow("up", None).await);
        // 冷却结束后：下一次 allow 进入 HalfOpen 并占用唯一探针
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(reg.allow("up", None).await);
        // 第二个并发请求在半开期间必须被拒绝（R3：单探针）
        assert!(!reg.allow("up", None).await);
        // 探针成功 → 闭合，恢复放行
        reg.record_success("up").await;
        assert!(reg.allow("up", None).await);
    }

    #[tokio::test]
    async fn per_route_threshold_override() {
        let reg = CircuitBreakerRegistry::new();
        // 路由级阈值 = 2 且独立于全局 5
        assert!(reg.allow("/api/a", Some(2)).await);
        reg.record_failure("/api/a").await;
        reg.record_failure("/api/a").await;
        assert_eq!(reg.state_of("/api/a").await, BreakerState::Open);
        // 全局阈值路径不受影响
        assert!(reg.allow("/api/b", None).await);
        assert_eq!(reg.state_of("/api/b").await, BreakerState::Closed);
    }

    #[tokio::test]
    async fn threshold_zero_disables_breaker() {
        let reg = CircuitBreakerRegistry::new();
        assert!(reg.allow("/api/no-cb", Some(0)).await);
        for _ in 0..100 {
            reg.record_failure("/api/no-cb").await;
        }
        assert_eq!(reg.state_of("/api/no-cb").await, BreakerState::Closed);
        assert!(reg.allow("/api/no-cb", Some(0)).await);
    }
}
