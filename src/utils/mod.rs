pub mod crypto;
pub mod error;
pub mod template;

pub use error::AppError;

/// 小写十六进制编码（`sha2` 0.11 起摘要输出不再实现 `LowerHex`，统一走此函数）。
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
