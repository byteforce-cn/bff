//! 真实验签契约测试（P1 遗留项「真实验签契约测试」）。
//!
//! 既有 mock IdP（`tests/common/mod.rs`）以 `alg: none` + `insecure_skip_id_token_verification: true`
//! 运行，**不经过 openidconnect 的真实验签路径**；Keycloak E2E（`deploy/keycloak/`）虽已实测
//! RS256/JWKS，但依赖 Docker 环境，不能在 `cargo test` 门禁中回归。
//!
//! 本文件构建**进程内 RS256 签名 IdP + JWKS**，并让 BFF 以完整校验（不跳过验签）运行：
//!
//! 1. 合法 RS256 签名且 JWKS 匹配 → 登录成功（真实验签主路径）；
//! 2. 使用与 JWKS 不匹配的密钥签名（伪造/密钥轮换攻击）→ 回调 401、不建会话；
//! 3. `alg: none` 无签名令牌（JWT 算法混淆攻击）→ 回调 401、不建会话；
//! 4. 签名合法但 nonce 与授权请求不一致（重放/串会话）→ 回调 401、不建会话。
//!
//! 回归价值：任何"验签开关被默认打开 / 验签被绕过 / nonce 校验被移除"的改动
//! 都会使 2–4 用例失败。

mod common;

use base64::Engine;
use bff::config::OidcProviderConfig;
use bff::state::AppState;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

// ============================================================
// 密钥（内嵌固定测试密钥，仅测试用途、非秘密；openssl genpkey 生成）
// —— 避免 debug 构建下 RSA 生成（~数秒/把）拖慢测试
// ============================================================

/// 签发密钥（与 JWKS 匹配）
const TEST_ISSUE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCnYg0UKYhiAoP+
D/O9UP92ohi0+iLADbK2XMbwk8ELd6muJSZv/TV/Up9zwcqwDxLMrOgivmyGh7Ni
Dm3iD+tzO0co9919JbzaVeNtx7veBC/GByFuDO7ywi5IkyJ6MVbKNnDIvCKqCpqx
gKqsKRQIU984kj5fnIFzoVcLo7LblZ5XOeceiUJoEqD9b1QG4MzJ6jM48FN12e5V
e2YiOMj41GfYwsumakPnNKnWWxcQ9bLHN6b+6v+iGZTXiypzkUFWxffl0//wl8Xc
GFHrzdTZGdyjDrzP7K0jQ37iurAZMnEk8bRFyx4RdMbD9Se4KLD4V0PA5sPhy2OB
23Ytlj7FAgMBAAECggEABRmI7foU+G18sYcxZS18VxQ5vfvYBrN0JDe/7PEHgu6u
qgpiOSvFDz/IcWmwX/xZlYhYG0TjgBbO2Zg4c1iKUyy1bpNcuXUmo51VzFC3Udyd
SwKJG3YD6rwNVnM7K+9oZklR1t/ai4U+sNVLferTfCx4Atx7z4RwckIyaX2fk66W
8QAysHd/91tPd22PjvVw6x1ttWmxCn0JqJuh14tVw49Quf89UbZD0NCidd4A8ng6
3CjwczYqMhsxIojkB3/rqnvL75CngOZHNm0kw75kNr4kIKvzbxkAw9BmNf+ZK2XF
rlUgb9IvDsnt7AIqGdOFegHYwSyeVvWqlxJtonDB2QKBgQDWeCUl9rwkLvKl9YlO
+OUmgrtJuCfkwMVsOz+ksLSSIvsxAmYkTi4wJAfotazX5iN4W7Whl7+RJPpKvMLs
/JLAF9A8egKnkrLXDK4KMK8Smox7MXlJbQmbVX7EQedEacA5GJpHpko5L7KPnZzm
CShPVgmVN35qduyGCqRFJlqkDQKBgQDHy7QoPvNdYlcqNofib5ToAnuzESBTP2gl
MmU8yg2+AzEpFDoPncbkJ6HmyeNhf5u6vRL6hJxc6+icGMqq+AKeTCNaQckx9iM6
THgBjSMv+TXMhYrLv2W7QSq/YpK3zz/Sr6O0rs8Vqyz0M3FnSCmoXokzdDzJ6Qbc
FsS9NXc/mQKBgQCvoniZnHP7Fc91BZ1K5R2T6h/CgWN6PDvxJJw8HNHjk24udo57
UOMWXYt0kcNYk4mcuU4HZaRmEug+aFMhjL4JPfc0b57Y6JQ49JNamP/mtlYxVRTE
gt0JLny/8FCagBgBKhq+bnn+VwdeAW9KG1m9jvIOFwIZ4gZUx0Y7susryQKBgHz6
7EOQvWPZNHVvjykSa6+GfiLRv8rTiy5ZjAKu0lHeZU4xHPDP3a6zLA/WkqpWzO/P
fqO/eKCX4fZje8PfSKQFNMgtBtJ+CiNZ2mf+Bdjop8K8dsplfBna9gaqfuUEfAQr
YtiP0XLYlVJdK79T4Ns159WMDMqxPl1G0OMbIvFhAoGBAMpUAvSgs3L0qrj697ri
mURgdbZwmxqPHFCcBITP0njSm3uzdfOVmtWI+on9Lb7sQZGgzDXS8PtUwagv1ozI
nn8ux/W2O3plzAoER0dbadm67PFIyF/VnHBCRZv/bUpBKw4nPQ8hwvzEDCLHSJrf
85Zon3XX1g5cuCKrb1mhRnaE
-----END PRIVATE KEY-----
";

/// 伪造密钥（与 JWKS 不匹配，用于密钥混淆攻击用例）
const TEST_WRONG_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDXD7c48H/yoAvw
I9VvD3K14DMt9nV81V9SjISmrk6ZaEJtqCslq/h3jp98duyi/ZFpRbKBFXte/BJ7
XSzLpzgMhsgxB2iMMX0dxZ2R1VsDBoX1jwDwpTysS/qV+NHYTnwQeFk3TWpuiMzm
ZM0ZKGUGw1zseHdeLHnumV4jzZ4DnByUq45zMThSneiTBWTnQoCaTdit90px5Yvn
FWGd7I+L5jZ8j5/saWF1bUpxSbPvI0kJJHRbowtX8OJXrcA3TjvfvtIetpquh5ws
8rfSINIJDA4ZWfayGPh03Emt9kG/jzHQE4OzXrucP9ocjTW78Fc3VaH+YOZCVSU3
JjaPC/btAgMBAAECggEAD4z0h9qlYyMH1J9ACCNQUplkaf7nuOqUKsZmsxy91Iw8
PWygY2gfc8RYK5T31cNLDUHRFKdmf9REoS9Nar5r0vAOBV7j7uXchldrO4fMvcJu
KPbkWleVtF8GvsBlVWQ7cVTFHWFs1ZLxFKRroNWRvzRHgavQ9Hoifkpltbtg9na5
yD4Y7iXWJZ94pv+0HI4FZF6wV9ESu6AtCXCpIr960+Gai4Ig4XI+lnaEfUdGLa6j
thLBKOFA2jjzzfyJ7C6dLsnw4JVIuTioaoiBIQkfXRv1SYBiA+z/HGiuDS0t1qio
TVyM2HI8Gm06vlVj21+z1x6El9e/9q+qFpnR50wlwQKBgQD+shFuazufujdGHW0i
P5E5jMhNRUwm0NbepFHqvy2kh8oLq8wAPTFEzzJlJFgYKGC9PMGX0iW94Lz6+GJm
jrsjw7M2uvQH/MiQyuOzi7waCBDJgeDKyppTqeXpPkUc5FyCQSkXxKkPFy5zw9Z4
p4e1sUmd0uq6BSfah57nvsk44QKBgQDYKa7jG7wjFQy3896OAFysYq67z+GuMXHA
vQWlV709QO7mqThKKfxe/F78G9eAFWhMrjU38Y7bKnwDKSVgu3+LrSHNCxoG0p/5
e4ZzMot8G7Rzd3u390lSgTvp8Dlna3aX3BWw2vp3yS4vJqMwwMVWtELdJ9UnSlnv
VF8v/y0DjQKBgQDE/vHgl+xcFOofvy7kKIqpGqzqp0jJVQp81lfN2+Tvt1+dO1nk
bXAoKqJt/Hhu5vw8IjwSs6YhgSxqaaeib49rkDiTgnKxouF2rJcGDnSFJevmECDQ
eXh4cZa0m0dVm4O587BXA/NHCsURIU5HsDyVWfT4r2SCUO6MZg2Qbc6xwQKBgAsf
OexdhPSZJKpiVdUgl6QW/76SF56K1LuB/kRfm1EHgkND+a13M5D/kzONiyz/7Pnl
DL/wIdWM/gx7lXzAqPNa2R5fr9siAzEm9ef/dcXQ9xvpzefNRWyFUbvbrFhx4ww1
Orh6y+BV7ZZneoYLpRus8rPGVOVMogv6X1ts2bgRAoGBAKE1Qbh1nzaWQk3Drq0i
46drNXQ4GIk3MKirPtiHjebnctOl+GNzO1DpeaK09byDBAKjMmaqCPzYik/evBc3
X3xldtZKGF/QpSYzJFUn8/VMJhI8i5hfpc6grgwicMKLW2nDEIL0EpE3SvPI6EeY
FjcFU6+ouI/z4wdjYm6DFtxC
-----END PRIVATE KEY-----
";

struct TestKeys {
    issue: RsaPrivateKey,
    wrong: RsaPrivateKey,
    jwks_public: RsaPublicKey,
}

fn test_keys() -> &'static TestKeys {
    static KEYS: OnceLock<TestKeys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let issue = RsaPrivateKey::from_pkcs8_pem(TEST_ISSUE_KEY_PEM).expect("解析测试密钥失败");
        let wrong = RsaPrivateKey::from_pkcs8_pem(TEST_WRONG_KEY_PEM).expect("解析测试密钥失败");
        let jwks_public = RsaPublicKey::from(&issue);
        TestKeys {
            issue,
            wrong,
            jwks_public,
        }
    })
}

// ============================================================
// 进程内 RS256 签名 IdP
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ForgeMode {
    /// 合法：签发密钥与 JWKS 匹配、nonce 一致
    Valid,
    /// 伪造：使用与 JWKS 不匹配的密钥签名
    WrongKey,
    /// `alg: none`：无签名令牌
    AlgoNone,
    /// 签名合法但 nonce 被替换
    WrongNonce,
}

#[derive(Clone)]
struct SigningIdpState {
    url: String,
    kid: String,
    /// 测试写入的 nonce（与授权请求一致才合法）
    nonce: Arc<Mutex<Option<String>>>,
    /// 签发模式（由测试在回调前设置）
    mode: Arc<Mutex<ForgeMode>>,
}

pub struct SigningIdp {
    pub url: String,
    pub nonce: Arc<Mutex<Option<String>>>,
    pub mode: Arc<Mutex<ForgeMode>>,
}

const KID: &str = "test-key-1";

fn b64_json(v: &serde_json::Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
}

fn rsa_jwk(pub_key: &RsaPublicKey, kid: &str) -> serde_json::Value {
    let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pub_key.n().to_bytes_be());
    let e = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pub_key.e().to_bytes_be());
    serde_json::json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": kid,
        "n": n,
        "e": e,
    })
}

/// 构造 id_token：按模式选择签名密钥 / 算法 / nonce。
fn make_id_token(st: &SigningIdpState, issued_nonce: &str, mode: ForgeMode) -> String {
    use rsa::pkcs1v15::SigningKey;
    use rsa::sha2::Sha256;
    use rsa::signature::{SignatureEncoding, Signer};

    let keys = test_keys();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let header = match mode {
        ForgeMode::AlgoNone => serde_json::json!({"alg": "none", "typ": "JWT", "kid": st.kid}),
        _ => serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": st.kid}),
    };
    let payload = serde_json::json!({
        "sub": "user-1",
        "iss": st.url,
        "aud": "bff-client",
        "exp": now + 3600,
        "iat": now - 60,
        "nonce": issued_nonce,
    });

    let signing_input = format!("{}.{}", b64_json(&header), b64_json(&payload));
    match mode {
        ForgeMode::AlgoNone => format!("{signing_input}."),
        ForgeMode::WrongKey => {
            let signer = SigningKey::<Sha256>::new(keys.wrong.clone());
            let sig = signer.sign(signing_input.as_bytes());
            format!(
                "{signing_input}.{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_vec())
            )
        }
        _ => {
            let signer = SigningKey::<Sha256>::new(keys.issue.clone());
            let sig = signer.sign(signing_input.as_bytes());
            format!(
                "{signing_input}.{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_vec())
            )
        }
    }
}

async fn spawn_signing_idp() -> SigningIdp {
    use axum::extract::State as AxState;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let st = SigningIdpState {
        url: url.clone(),
        kid: KID.to_string(),
        nonce: Arc::new(Mutex::new(None)),
        mode: Arc::new(Mutex::new(ForgeMode::Valid)),
    };

    async fn discovery(AxState(st): AxState<SigningIdpState>) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "issuer": st.url,
            "authorization_endpoint": format!("{}/authorize", st.url),
            "token_endpoint": format!("{}/token", st.url),
            "jwks_uri": format!("{}/jwks", st.url),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            // 仅公布 RS256：验签器据此拒绝 alg:none / HS256 等
            "id_token_signing_alg_values_supported": ["RS256"],
        }))
    }

    async fn jwks(AxState(st): AxState<SigningIdpState>) -> Json<serde_json::Value> {
        // 仅公布签发公钥（与 WrongKey 模式的伪造密钥刻意不同）
        Json(serde_json::json!({
            "keys": [rsa_jwk(&test_keys().jwks_public, &st.kid)]
        }))
    }

    async fn token(
        AxState(st): AxState<SigningIdpState>,
        axum::Form(form): axum::Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let grant = form.get("grant_type").cloned().unwrap_or_default();
        match grant.as_str() {
            "authorization_code" => {
                let mode = *st.mode.lock().unwrap();
                let nonce = st.nonce.lock().unwrap().clone().unwrap_or_default();
                let issued_nonce = if mode == ForgeMode::WrongNonce {
                    "forged-nonce"
                } else {
                    nonce.as_str()
                };
                Json(serde_json::json!({
                    "access_token": "signed-access-token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "signed-refresh-token",
                    "id_token": make_id_token(&st, issued_nonce, mode),
                }))
            }
            other => Json(serde_json::json!({
                "error": "unsupported_grant_type",
                "error_description": other,
            })),
        }
    }

    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/jwks", get(jwks))
        .with_state(st.clone());

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    SigningIdp {
        url,
        nonce: st.nonce,
        mode: st.mode,
    }
}

/// 指向签名 IdP 的 provider 配置：**不跳过验签**（完整 RS256/JWKS 校验）。
fn signing_provider_cfg(idp: &SigningIdp) -> OidcProviderConfig {
    OidcProviderConfig {
        id: "signed".into(),
        display_name: "Signed IdP".into(),
        issuer_url: idp.url.clone(),
        client_id: "bff-client".into(),
        client_secret: "bff-secret".into(),
        callback_path: "/auth/callback".into(),
        scopes: vec!["openid".into()],
        insecure_skip_id_token_verification: false,
        refresh_skew_secs: 60,
        shared_across_sites: false,
    }
}

/// 驱动一次完整登录（/login → 伪造回调），返回回调状态码与 AppState。
async fn drive_login(idp: &SigningIdp, mode: ForgeMode) -> (reqwest::StatusCode, AppState) {
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(signing_provider_cfg(idp));
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state.clone()).await;
    let client = common::test_client();

    // 1. /login → 提取 state/nonce（与真实 IdP 相同路径）
    let resp = client.get(format!("{}/login", bff)).send().await.unwrap();
    assert!(resp.status().is_redirection(), "应重定向到签名 IdP");
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    let auth_url = url::Url::parse(&location).unwrap();
    let params: HashMap<_, _> = auth_url.query_pairs().into_owned().collect();
    let state_param = params.get("state").expect("授权 URL 应含 state").clone();
    let nonce = params.get("nonce").expect("授权 URL 应含 nonce").clone();
    *idp.nonce.lock().unwrap() = Some(nonce);
    *idp.mode.lock().unwrap() = mode;

    // 2. 模拟 IdP 回调（token 端点返回按模式构造的 id_token）
    let resp = client
        .get(format!(
            "{}/auth/callback?code=real-sign&state={}",
            bff, state_param
        ))
        .send()
        .await
        .unwrap();
    (resp.status(), state)
}

async fn session_count(state: &AppState) -> usize {
    state.sessions.read().await.len()
}

// ============================================================
// 1. 合法 RS256 + JWKS 匹配 → 登录成功
// ============================================================

#[tokio::test]
async fn rs256_jwks_valid_login_succeeds() {
    let idp = spawn_signing_idp().await;
    let (status, state) = drive_login(&idp, ForgeMode::Valid).await;
    assert!(
        status.is_redirection(),
        "合法 RS256 签名应通过验签并重定向: {status}"
    );
    assert_eq!(session_count(&state).await, 1, "应建立会话");
    let sessions: Vec<_> = state.sessions.read().await.values().cloned().collect();
    assert_eq!(sessions[0].provider, "signed");
    assert_eq!(sessions[0].sub, "user-1");
}

// ============================================================
// 2–4. 攻击面：伪造密钥 / alg:none / nonce 不一致 → 401 且不建会话
// ============================================================

#[tokio::test]
async fn rs256_wrong_key_rejected() {
    let idp = spawn_signing_idp().await;
    let (status, state) = drive_login(&idp, ForgeMode::WrongKey).await;
    assert_eq!(status, 401, "签名密钥与 JWKS 不匹配必须拒绝");
    assert_eq!(session_count(&state).await, 0, "验签失败不得建立会话");
}

#[tokio::test]
async fn rs256_alg_none_rejected() {
    let idp = spawn_signing_idp().await;
    let (status, state) = drive_login(&idp, ForgeMode::AlgoNone).await;
    assert_eq!(status, 401, "alg:none 无签名令牌必须拒绝（JWT 混淆攻击）");
    assert_eq!(session_count(&state).await, 0, "验签失败不得建立会话");
}

#[tokio::test]
async fn rs256_wrong_nonce_rejected() {
    let idp = spawn_signing_idp().await;
    let (status, state) = drive_login(&idp, ForgeMode::WrongNonce).await;
    assert_eq!(status, 401, "签名合法但 nonce 不一致必须拒绝（重放防护）");
    assert_eq!(session_count(&state).await, 0, "nonce 校验失败不得建立会话");
}

// ============================================================
// 5. 契约健全性：测试中的 JWKS 可被 wiremock 风格断言读取（防手误）
// ============================================================

#[tokio::test]
async fn idp_jwks_contract_is_served_and_parseable() {
    let idp = spawn_signing_idp().await;
    let client = common::test_client();
    let resp = client
        .get(format!("{}/jwks", idp.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let key = &body["keys"][0];
    assert_eq!(key["kty"], "RSA");
    assert_eq!(key["alg"], "RS256");
    assert_eq!(key["kid"], KID);
    assert!(key["n"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
    assert!(key["e"].as_str().map(|s| !s.is_empty()).unwrap_or(false));

    // discovery 只公布 RS256（验签器据此拒绝其它算法）
    let disc: serde_json::Value = client
        .get(format!("{}/.well-known/openid-configuration", idp.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        disc["id_token_signing_alg_values_supported"],
        serde_json::json!(["RS256"])
    );
}
