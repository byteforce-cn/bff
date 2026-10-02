# syntax=docker/dockerfile:1

# ============================================================
# Stage 1: 构建 admin-ui（RustEmbed 编译期强依赖 admin-ui/dist）
# ============================================================
FROM node:22-alpine AS admin-ui
RUN corepack enable
WORKDIR /app/admin-ui
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml ./
# --ignore-scripts：新版 pnpm 默认拦截依赖构建脚本（esbuild 等），
# esbuild 二进制由平台可选依赖提供，无需 postinstall
RUN pnpm install --frozen-lockfile --ignore-scripts
COPY admin-ui/ ./
RUN pnpm build

# ============================================================
# Stage 2: 构建演示 SPA（业务端口静态资源 frontend/dist）
# ============================================================
FROM node:22-alpine AS frontend
RUN corepack enable
WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts
COPY frontend/ ./
RUN pnpm build

# ============================================================
# Stage 3: 编译 Rust 二进制
# ============================================================
FROM rust:1.93-slim-bookworm AS builder
WORKDIR /app

# 可选构建参数：cargo 注册表镜像（网络受限环境，如：
#   --build-arg CARGO_MIRROR="sparse+https://rsproxy.cn/index/"
# CI/公网环境留空即使用 crates.io）。
ARG CARGO_MIRROR=""
RUN if [ -n "$CARGO_MIRROR" ]; then \
      printf '[source.crates-io]\nreplace-with = "mirror"\n[source.mirror]\nregistry = "%s"\n' "$CARGO_MIRROR" > /usr/local/cargo/config.toml; \
    fi

# 先拷贝清单以利用依赖缓存
COPY Cargo.toml Cargo.lock ./
# 最小 src 占位以缓存依赖编译（target 层在 src 变更时仍可复用）
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
    && mkdir -p admin-ui/dist && echo '<!doctype html><title>placeholder</title>' > admin-ui/dist/index.html \
    && cargo build --release --locked 2>/dev/null || true

# 真实源码 + UI 产物
COPY src ./src
COPY --from=admin-ui /app/admin-ui/dist ./admin-ui/dist
RUN touch src/main.rs && cargo build --release --locked

# ============================================================
# Stage 4: 运行镜像（最小化，无 apt 依赖）
# ============================================================
# 说明：不安装 curl/tini/ca-certificates——
# - 运行期 TLS 走 rustls（bundled webpki-roots），自定义 CA 由配置挂载；
# - BFF 无子进程，无需 tini；进程本身处理 SIGTERM 优雅停机；
# - 容器探针在编排层实现（K8s 用 HTTP 探针；compose 用 redis 健康门控）。
FROM debian:bookworm-slim AS runtime

WORKDIR /app
COPY --from=builder /app/target/release/bff /usr/local/bin/bff
# 声明式配置（生产可由 K8s ConfigMap 挂载覆盖 /app/config）
COPY config ./config
COPY --from=frontend /app/frontend/dist ./frontend/dist

# 配置持久化目录（具名卷初始化时继承该目录的属主 → 非 root 进程可写）
RUN mkdir -p /data/bff && chown 10001:10001 /data/bff

# 数值 UID（无需 useradd）
USER 10001:10001
EXPOSE 8080 8443

ENTRYPOINT ["/usr/local/bin/bff"]
