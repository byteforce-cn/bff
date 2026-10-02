# fakesvc — 本地下游服务（开发/测试组件）

Spring Boot 3 编写的 **测试用** 下游服务（端口 `:9091`）：提供用户 / 订单 / SSE / WebSocket 等接口，
配合 `config/routes/routes.yaml` 与 `config/pipelines/example.yaml` 的示例做本地联调。

> ⚠️ 这是开发/测试夹具，**不是生产组件**。

## 运行

```bash
mvn spring-boot:run
```

接口与用途见 `src/main/java/cn/byteforce/bff/dev/fakesvc/controller/`。
