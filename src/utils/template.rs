//! URL / 字符串模板替换：`{name}` 占位符从参数表取值，并做 URL 编码。
use std::collections::HashMap;

pub fn render(template: &str, params: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match params.get(key) {
                    Some(v) => out.push_str(&urlencoding(v)),
                    // 未提供的占位符做 URL 编码保留，避免输出非法字符（{ }）导致下游
                    // Tomcat/反向代理拒绝请求（RFC 7230/3986）。
                    None => {
                        out.push_str(&urlencoding(&format!("{{{}}}", key)));
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn urlencoding(s: &str) -> String {
    // 路径段编码：空格 -> %20（非 form 的 +）
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(s, SET).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_placeholders() {
        let mut p = HashMap::new();
        p.insert("userId".to_string(), "1 a".to_string());
        assert_eq!(
            render("http://x/users/{userId}?q={q}", &p),
            "http://x/users/1%20a?q=%7Bq%7D"
        );
    }
}
