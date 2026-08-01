import { defineConfig } from "vite";

export default defineConfig({
  server: {
    port: 4173,
    proxy: {
      // 将所有非静态请求代理到 BFF 业务端口
      "/login": "http://127.0.0.1:8080",
      "/logout": "http://127.0.0.1:8080",
      "/auth": "http://127.0.0.1:8080",
      "/health": "http://127.0.0.1:8080",
      "/pipeline": "http://127.0.0.1:8080",
      "/api": "http://127.0.0.1:8080",
      // fakesvc 全功能代理
      "/sse": "http://127.0.0.1:8080",
      "/ws": {
        target: "ws://127.0.0.1:8080",
        ws: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
