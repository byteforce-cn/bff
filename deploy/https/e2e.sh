#!/usr/bin/env bash
# HTTPS 全链路 E2E 验收脚本：登录 → 回调 → 会话 → 登出（RP-Initiated Logout）
#
# 前置：
#   1) `cargo run --release --example mock_idp`（宿主，9090）
#   2) `docker compose -f docker-compose.yml -f deploy/https/docker-compose.https.yml up -d`
#   3) 等待 BFF 就绪（脚本内自检）
#
# 用法：bash deploy/https/e2e.sh [https://localhost:9443]
set -euo pipefail

BASE="${1:-https://localhost:9443}"
JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT
CURL=(curl -sk --max-time 15)

pass() { echo "  ✅ $1"; }
fail() { echo "  ❌ $1"; exit 1; }

echo "== 0) 等待 $BASE/live 就绪"
for _ in $(seq 1 30); do
  if [[ "$("${CURL[@]}" -o /dev/null -w '%{http_code}' "$BASE/live" || true)" == "200" ]]; then
    break
  fi
  sleep 1
done
[[ "$("${CURL[@]}" -o /dev/null -w '%{http_code}' "$BASE/live")" == "200" ]] || fail "BFF /live 未就绪"
pass "/live = 200（经 nginx TLS 终止）"

echo "== 1) redirect_uri 必须基于 public_base_url（防 Host 污染，P0-2）"
LOC="$("${CURL[@]}" -D- -o /dev/null "$BASE/login" | tr -d '\r' | awk 'tolower($1)=="location:"{print $2}')"
echo "$LOC" | grep -q "redirect_uri=https%3A%2F%2Flocalhost%3A9443%2Fauth%2Fcallback" \
  || fail "redirect_uri 未按 public_base_url 推导: $LOC"
pass "登录重定向含固定 redirect_uri（https://localhost:9443/auth/callback）"

# P0-2 验收：伪造 Host 并发触发后 redirect_uri 仍恒定
for _ in 1 2 3; do
  LOC_EVIL="$("${CURL[@]}" -H 'Host: evil.example.com' -D- -o /dev/null "$BASE/login" | tr -d '\r' | awk 'tolower($1)=="location:"{print $2}')"
  echo "$LOC_EVIL" | grep -q "redirect_uri=https%3A%2F%2Flocalhost%3A9443%2Fauth%2Fcallback" \
    || fail "伪造 Host 污染了 redirect_uri: $LOC_EVIL"
done
pass "伪造 Host x3 后 redirect_uri 仍为 public_base_url 推导值"

echo "== 2) 完整登录（跟随 Mock IdP 自动授权回跳）"
CODE="$("${CURL[@]}" -c "$JAR" -b "$JAR" -L -o /dev/null -w '%{http_code}' "$BASE/login")"
[[ "$CODE" == "200" ]] || fail "登录链路最终状态 $CODE"
SESSION="$("${CURL[@]}" -b "$JAR" "$BASE/api/session")"
echo "$SESSION" | grep -q '"logged_in":true' || fail "登录后会话状态异常: $SESSION"
pass "登录成功，/api/session = $SESSION"

echo "== 3) 登出（discovery end_session_endpoint + 回跳）"
CODE="$("${CURL[@]}" -c "$JAR" -b "$JAR" -L -o /dev/null -w '%{http_code}' "$BASE/logout")"
[[ "$CODE" == "200" ]] || fail "登出链路最终状态 $CODE"
SESSION="$("${CURL[@]}" -b "$JAR" "$BASE/api/session")"
echo "$SESSION" | grep -q '"logged_in":false' || fail "登出后会话状态异常: $SESSION"
pass "登出成功，/api/session = $SESSION"

echo ""
echo "🎉 HTTPS 全链路 E2E 通过（登录/回调/会话/登出；经 LB TLS 终止）"
