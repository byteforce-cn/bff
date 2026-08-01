//! 场景 2：SPA 静态文件服务及 fallback。
mod common;

#[tokio::test]
async fn spa_serving_and_fallback() {
    let mut cfg = common::base_config();
    cfg.spa.dir = common::make_spa_dir("spa");
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 1. /index.html → 200 text/html
    let resp = client.get(format!("{}/index.html", bff)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("text/html"), "content-type: {}", ct);
    assert!(resp.text().await.unwrap().contains("spa"));

    // 2. 静态资源 /app.js → 200
    let resp = client.get(format!("{}/app.js", bff)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("console.log"));

    // 3. 前端路由 fallback → index.html
    let resp = client.get(format!("{}/dashboard/settings", bff)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("text/html"), "fallback content-type: {}", ct);
    assert!(resp.text().await.unwrap().contains("spa"));

    // 4. /api 下未命中路由 → 404（不回退到 index.html）
    let resp = client.get(format!("{}/api/nonexistent", bff)).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}
