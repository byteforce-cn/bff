//! 敏感字段加密（AES-256-GCM）。
//!
//! 密钥派生：使用 Argon2id 从配置中的 `bff_secret.secret` 和 `bff_secret.salt` 派生。
//! 启动时由 `AppState::new()` 调用 `crypto::init()` 完成初始化。
//!
//! 未初始化时加密/解密操作将 panic，确保不会在生产中遗漏配置。

use aes_gcm::aead::{Aead, Generate, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::Argon2;
use base64::Engine;
use std::sync::OnceLock;

/// AES-GCM 标准 nonce 长度（96 bit）。
const NONCE_LEN: usize = 12;

/// 全局密钥缓存：由 `init()` 在启动时初始化一次。
static KEY_CACHE: OnceLock<Result<Aes256Gcm, String>> = OnceLock::new();

/// 启动时调用：从 secret 和 salt 派生 AES-256 密钥（Argon2id，64 MiB，3 迭代，4 并行度）。
pub fn init(secret: &str, salt: &str) -> Result<(), String> {
    if secret.is_empty() {
        return Err("bff_secret.secret 为空，拒绝使用弱密钥".to_string());
    }
    if salt.len() < 16 {
        return Err(format!(
            "bff_secret.salt 长度不足（{} 字节，需要 ≥16 字节）",
            salt.len()
        ));
    }

    let mut key_bytes = [0u8; 32];
    Argon2::default()
        .hash_password_into(secret.as_bytes(), salt.as_bytes(), &mut key_bytes)
        .map_err(|e| format!("Argon2 密钥派生失败: {}", e))?;

    let key = Key::<Aes256Gcm>::from(key_bytes);
    let _ = KEY_CACHE.set(Ok(Aes256Gcm::new(&key)));

    tracing::info!("AES-256-GCM 密钥已初始化");
    Ok(())
}

/// 获取已初始化的 AES-256-GCM 加密器（未初始化时 panic）。
fn cipher() -> &'static Aes256Gcm {
    KEY_CACHE
        .get()
        .expect("crypto::init() 未被调用，启动流程异常")
        .as_ref()
        .expect("crypto::init() 失败，密钥不可用")
}

/// 使用 AES-256-GCM 加密明文，返回 base64url 编码的 nonce+密文。
pub fn encrypt(plaintext: &[u8]) -> anyhow::Result<String> {
    let c = cipher();
    let nonce = Nonce::generate();
    let ct = c
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("加密失败: {}", e))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out))
}

/// 解密 base64url 编码的 nonce+密文，返回明文。
pub fn decrypt(encoded: &str) -> anyhow::Result<Vec<u8>> {
    let c = cipher();
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|e| anyhow::anyhow!("base64 解码失败: {}", e))?;
    let n = NONCE_LEN;
    anyhow::ensure!(raw.len() > n, "密文长度非法");
    let (nonce, ct) = raw.split_at(n);
    let nonce_arr: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| anyhow::anyhow!("nonce 长度非法"))?;
    c.decrypt(&Nonce::from(nonce_arr), ct)
        .map_err(|e| anyhow::anyhow!("解密失败: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        init("my-super-secret-key-for-testing", "abcdefghijklmnop").expect("初始化应成功");
        let plaintext = b"hello, world! this is a test message";
        let encrypted = encrypt(plaintext).expect("加密应成功");
        let decrypted = decrypt(&encrypted).expect("解密应成功");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_empty_plaintext() {
        init("another-secret-key-for", "1234567890123456").expect("初始化应成功");
        let encrypted = encrypt(b"").expect("空明文加密应成功");
        let decrypted = decrypt(&encrypted).expect("解密应成功");
        assert_eq!(decrypted, b"");
    }

    #[test]
    fn test_decrypt_tampered_ciphertext() {
        init("test-key-1234567890", "saltsaltsaltsalt").expect("初始化应成功");
        let encrypted = encrypt(b"sensitive data").expect("加密应成功");
        use base64::Engine;
        let mut raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&encrypted)
            .expect("base64 decode");
        let nonce_len = NONCE_LEN; // AES-GCM nonce
        if raw.len() > nonce_len {
            raw[nonce_len] ^= 0x01;
        }
        let tampered = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
        let result = decrypt(&tampered);
        assert!(result.is_err(), "篡改密文解密应失败");
    }

    #[test]
    fn test_different_nonces_produce_different_ciphertexts() {
        init("key-one-abcdefghij", "salt11111111111111").expect("初始化应成功");
        let ct1 = encrypt(b"same plaintext").expect("加密应成功");
        let ct2 = encrypt(b"same plaintext").expect("加密应成功");
        assert_ne!(ct1, ct2, "不同 nonce 应产生不同密文");
    }

    #[test]
    fn test_init_rejects_short_salt() {
        assert!(init("secret", "short").is_err());
    }

    #[test]
    fn test_init_rejects_empty_secret() {
        assert!(init("", "1234567890123456").is_err());
    }
}
