#!/usr/bin/env bash
# 生成本地 HTTPS E2E 自签证书（仅测试用）
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/certs"
mkdir -p "$DIR"

if [[ -f "$DIR/server.crt" && -f "$DIR/server.key" ]]; then
  echo "证书已存在: $DIR/server.crt"
  exit 0
fi

openssl req -x509 -nodes -newkey rsa:2048 -days 365 \
  -keyout "$DIR/server.key" \
  -out "$DIR/server.crt" \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"

echo "已生成自签证书: $DIR/server.crt"
