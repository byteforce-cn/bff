//! 轻量熔断器：按 upstream 维度统计失败，超阈值后短路一段时间。
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
    failures: u32,
    opened_at: Option<Instant>,
}

impl Default for Breaker {
    fn default() -> Self {
        Self {
            state: BreakerState::Closed,
            failures: 0,
            opened_at: None,
        }
    }
}

#[derive(Clone)]
pub struct CircuitBreakerRegistry {
    inner: Arc<Mutex<HashMap<String, Breaker>>>,
    failure_threshold: u32,
    open_duration: Duration,
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            failure_threshold: 5,
            open_duration: Duration::from_secs(30),
        }
    }
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用自定义参数创建熔断器注册表。
    pub fn new_with_config(failure_threshold: u32, open_duration: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            failure_threshold,
            open_duration,
        }
    }

    /// 调用前检查：Open 状态且未到冷却期则拒绝。
    pub async fn allow(&self, upstream: &str) -> bool {
        let mut map = self.inner.lock().await;
        let b = map.entry(upstream.to_string()).or_default();
        let open_dur = self.open_duration;
        match b.state {
            BreakerState::Closed | BreakerState::HalfOpen => true,
            BreakerState::Open => {
                if b.opened_at
                    .map(|t| t.elapsed() >= open_dur)
                    .unwrap_or(false)
                {
                    b.state = BreakerState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub async fn record_success(&self, upstream: &str) {
        let mut map = self.inner.lock().await;
        let b = map.entry(upstream.to_string()).or_default();
        b.state = BreakerState::Closed;
        b.failures = 0;
        b.opened_at = None;
    }

    pub async fn record_failure(&self, upstream: &str) {
        let mut map = self.inner.lock().await;
        let b = map.entry(upstream.to_string()).or_default();
        let threshold = self.failure_threshold;
        // HalfOpen 下任何失败立即重新熔断
        b.failures = if b.state == BreakerState::HalfOpen {
            threshold
        } else {
            b.failures + 1
        };
        if b.failures >= threshold {
            b.state = BreakerState::Open;
            b.opened_at = Some(Instant::now());
            metrics::counter!("bff_circuit_breaker_open_total", "upstream" => upstream.to_string())
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
        for _ in 0..threshold {
            reg.record_failure("up").await;
        }
        assert_eq!(reg.state_of("up").await, BreakerState::Open);
        assert!(!reg.allow("up").await);
        reg.record_success("up").await;
        assert_eq!(reg.state_of("up").await, BreakerState::Closed);
    }
}
