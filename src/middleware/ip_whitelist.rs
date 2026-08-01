//! 管理端口 IP 白名单中间件。
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

/// 预解析的白名单（CIDR 或单个 IP）。
#[derive(Clone, Default)]
pub struct IpWhitelist {
    nets: Arc<Vec<IpNet>>,
}

impl IpWhitelist {
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        let mut nets = Vec::new();
        for e in entries {
            let net: IpNet = if e.contains('/') {
                e.parse().map_err(|_| format!("非法 CIDR: {}", e))?
            } else {
                let ip: IpAddr = e.parse().map_err(|_| format!("非法 IP: {}", e))?;
                IpNet::from(ip)
            };
            nets.push(net);
        }
        Ok(Self {
            nets: Arc::new(nets),
        })
    }

    pub fn allows(&self, ip: &IpAddr) -> bool {
        self.nets.iter().any(|n| n.contains(ip))
    }
}

pub async fn ip_whitelist_middleware(
    whitelist: IpWhitelist,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    match ip {
        Some(ip) if whitelist.allows(&ip) => next.run(req).await,
        Some(ip) => {
            tracing::warn!(%ip, "管理端口拒绝非白名单 IP");
            (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": "Forbidden: IP 不在白名单"})),
            )
                .into_response()
        }
        None => (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"error": "Forbidden: 无法确定来源 IP"})),
        )
            .into_response(),
    }
}
