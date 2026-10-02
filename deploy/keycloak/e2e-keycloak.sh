#!/usr/bin/env bash
# ============================================================
# Keycloak 真实 IdP 契约验证 E2E（一键，生产形态栈）
#
# 拓扑：
#   curl/浏览器 ──https:9443──▶ nginx(TLS 终止) ──▶ bff:8080（BFF_ENV=prod + Redis）
#     ├─ OIDC：discovery / 授权码+PKCE(S256) / code 换 token / RS256+JWKS 验签 / nonce
#     ├─ RP-Initiated Logout（discovery end_session_endpoint + id_token_hint）
#     └─ /api/echo ──▶ 回显上游（断言 Bearer 注入的是真实 Keycloak access token）
#
# 额外断言：
#   - 会话在 Redis：bff 容器重启后登录态不丢
#   - 令牌刷新：realm AT 寿命 90s + skew 60s，登录 ~35s 后 SWR 后台刷新，
#     上游侧观察到 access token 轮换、Keycloak 侧观察到 REFRESH_TOKEN 事件
#
# 用法：
#   bash deploy/keycloak/e2e-keycloak.sh                 # 复用已有 bff:local 镜像
#   FORCE_BUILD=1 bash deploy/keycloak/e2e-keycloak.sh   # 强制重建镜像
#   KEEP_STACK=1  bash deploy/keycloak/e2e-keycloak.sh   # 保留栈现场（排查用）
#
# 前置：docker / docker compose / curl / openssl；首次运行会拉取 Keycloak 镜像。
# 说明：脚本对 host.docker.internal（Keycloak）显式 --resolve 到 127.0.0.1；
#       真实浏览器访问时可在宿主机 /etc/hosts 增加 `127.0.0.1 host.docker.internal`。
# ============================================================
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

BASE="${BASE:-https://localhost:9443}"
KC_ADMIN_BASE="${KC_ADMIN_BASE:-http://127.0.0.1:8180}"
KC_REALM="bff"
KC_ISSUER="http://host.docker.internal:8180/realms/${KC_REALM}"
ADMIN_API="http://127.0.0.1:8443/admin/api/v1"

# 生产防呆所需密钥：脚本内生成并保持本次运行内固定（容器重启后会话仍可解密）。
# 已导出的值需通过最小长度校验，否则视为无效并重新生成（防误用短值）。
gen_secret() { # $1=变量名 $2=openssl rand 参数 $3=最小长度
  local name="$1" args="$2" min="$3" cur
  cur="${!name:-}"
  if [[ ${#cur} -ge $min ]]; then printf '%s' "$cur"; else openssl rand $args; fi
}
export BFF_ADMIN_TOKEN="$(gen_secret BFF_ADMIN_TOKEN '-hex 32' 32)"
export BFF_SECRET="$(gen_secret BFF_SECRET '-hex 32' 32)"
export BFF_SECRET_SALT="$(gen_secret BFF_SECRET_SALT '-hex 16' 16)"

COMPOSE=(
  docker compose
  -f docker-compose.yml
  -f deploy/https/docker-compose.https.yml
  -f deploy/keycloak/docker-compose.keycloak.yml
)

# Keycloak 与浏览器侧同名访问（容器经 host-gateway；宿主机脚本经 --resolve）
CURL=(curl -sk --max-time 30 --resolve host.docker.internal:8180:127.0.0.1)
JAR="$(mktemp)"
HTML="$(mktemp)"
HDR="$(mktemp)"
trap 'rm -f "$JAR" "$HTML" "$HDR"' EXIT

pass() { echo "  ✅ $1"; }
fail() {
  echo "  ❌ $1"
  echo "  —— 排查线索：KEEP_STACK=1 bash $0 保留现场；${COMPOSE[*]} logs <service> ——"
  exit 1
}
step() { echo ""; echo "== $1"; }

# JWT payload 解码（不依赖 jq/python）
jwt_payload() {
  local part
  part="$(printf '%s' "$1" | cut -d. -f2 | tr '_-' '/+')"
  case $((${#part} % 4)) in
    2) part+="==" ;;
    3) part+="=" ;;
  esac
  printf '%s' "$part" | base64 -d 2>/dev/null || true
}

kc_admin_token() {
  curl -s --max-time 10 -X POST "$KC_ADMIN_BASE/realms/master/protocol/openid-connect/token" \
    -d grant_type=password -d client_id=admin-cli -d username=admin -d password=admin |
    sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p'
}

kc_event_count() { # $1 = 事件类型
  local tok
  tok="$(kc_admin_token)"
  curl -s --max-time 10 -H "Authorization: Bearer $tok" \
    "$KC_ADMIN_BASE/admin/realms/${KC_REALM}/events?type=$1&max=200" |
    { grep -o "\"type\":\"$1\"" || true; } | wc -l | tr -d ' '
}

http_code() { # 打印 HTTP 状态码（curl 失败时输出 000）
  curl -sk --max-time 30 --resolve host.docker.internal:8180:127.0.0.1 \
    -o /dev/null -w '%{http_code}' "$@" 2>/dev/null || echo "000"
}

# ------------------------------------------------------------
step "0) 启动栈（nginx TLS 终止 + BFF[prod+Redis] + Keycloak 26）"
[[ -f deploy/https/certs/server.crt && -f deploy/https/certs/server.key ]] ||
  bash deploy/https/gen-certs.sh

BUILD_ARGS=()
if [[ "${FORCE_BUILD:-0}" == "1" ]] || ! docker image inspect bff:local >/dev/null 2>&1; then
  BUILD_ARGS=(--build)
fi
"${COMPOSE[@]}" up -d "${BUILD_ARGS[@]}" >/dev/null

echo "  等待 Keycloak discovery 就绪（最长 120s）..."
KC_OK=0
for _ in $(seq 1 120); do
  if curl -sf --max-time 3 "$KC_ADMIN_BASE/realms/$KC_REALM/.well-known/openid-configuration" >/dev/null 2>&1; then
    KC_OK=1
    break
  fi
  sleep 1
done
[[ $KC_OK == 1 ]] || fail "Keycloak 未在 120s 内就绪"
pass "Keycloak 就绪（realm=$KC_REALM）"

echo "  等待 BFF /live（经 nginx，最长 90s）..."
BFF_OK=0
for _ in $(seq 1 90); do
  if [[ "$(http_code "$BASE/live")" == "200" ]]; then
    BFF_OK=1
    break
  fi
  sleep 1
done
[[ $BFF_OK == 1 ]] || fail "BFF /live 未就绪"
pass "BFF 就绪（BFF_ENV=prod 全量防呆 + Redis provider）"

# ------------------------------------------------------------
step "1) Keycloak discovery 契约（issuer / 端点 / PKCE）"
DOC="$(curl -sf --max-time 10 "$KC_ADMIN_BASE/realms/$KC_REALM/.well-known/openid-configuration")"
echo "$DOC" | grep -q "\"issuer\":\"$KC_ISSUER\"" ||
  fail "discovery issuer 与 BFF 配置不一致（expect=$KC_ISSUER）"
echo "$DOC" | grep -q '"end_session_endpoint"' || fail "discovery 缺少 end_session_endpoint"
echo "$DOC" | grep -q 'S256' || fail "discovery 未声明 PKCE S256"
echo "$DOC" | grep -q '"token_endpoint_auth_methods_supported":\[[^]]*"client_secret_post"' ||
  fail "discovery 未声明 client_secret_post"
pass "discovery：issuer=$KC_ISSUER，end_session_endpoint / S256 / client_secret_post 齐备"

# BFF 侧经容器网络完成真实 discovery（provider verify 端点）
VERIFY="$(curl -s --max-time 15 -X POST -H "X-Admin-Token: $BFF_ADMIN_TOKEN" \
  "$ADMIN_API/oidc/providers/keycloak/verify")"
echo "$VERIFY" | grep -q '"ok":true' || fail "BFF provider verify 失败（容器→Keycloak）：$VERIFY"
pass "BFF 容器内经 host-gateway 完成 Keycloak discovery（verify ok）"

# ------------------------------------------------------------
step "2) 登录重定向（public_base_url 防 Host 污染 + PKCE S256）"
LOC="$("${CURL[@]}" -D- -o /dev/null "$BASE/login" | tr -d '\r' | awk 'tolower($1)=="location:"{print $2}')"
echo "$LOC" | grep -q 'redirect_uri=https%3A%2F%2Flocalhost%3A9443%2Fauth%2Fcallback' ||
  fail "redirect_uri 非 public_base_url 推导值: $LOC"
echo "$LOC" | grep -q 'code_challenge_method=S256' || fail "授权请求缺少 PKCE S256: $LOC"
# scope 必须为去重后的 openid+profile+email（openid 由 openidconnect crate 隐式注入一次）
echo "$LOC" | grep -q 'scope=openid+profile+email' || fail "scope 异常（重复/缺失）: $LOC"
echo "$LOC" | grep -q 'host.docker.internal:8180/realms/bff/protocol/openid-connect/auth' ||
  fail "授权端点不是 Keycloak realm 端点: $LOC"
for _ in 1 2; do # 伪造 Host 重放，redirect_uri 必须恒定
  LOC_EVIL="$("${CURL[@]}" -H 'Host: evil.example.com' -D- -o /dev/null "$BASE/login" |
    tr -d '\r' | awk 'tolower($1)=="location:"{print $2}')"
  echo "$LOC_EVIL" | grep -q 'redirect_uri=https%3A%2F%2Flocalhost%3A9443%2Fauth%2Fcallback' ||
    fail "伪造 Host 污染了 redirect_uri: $LOC_EVIL"
done
pass "redirect_uri 恒为 https://localhost:9443/auth/callback（伪造 Host ×2 未污染）"

# ------------------------------------------------------------
step "3) 真实登录（Keycloak 登录页 → 凭据提交 → 回调 → 会话）"
"${CURL[@]}" -c "$JAR" -b "$JAR" -L -D "$HDR" -o "$HTML" "$BASE/login"
# 跨站点 IdP 时会话 Cookie 需随回调（跨站顶层导航）携带：SameSite 必须为 Lax
# （Strict 会被浏览器丢弃 → 回调报“授权流程不存在”；curl 不强制 SameSite，
#  此处为属性级回归护栏，浏览器行为由 SameSite 规范保证）。
COOKIE="$(grep -i '^set-cookie: BFF_SESSION' "$HDR" | head -1 || true)"
echo "$COOKIE" | grep -qi 'HttpOnly' || fail "会话 Cookie 缺少 HttpOnly: $COOKIE"
echo "$COOKIE" | grep -qi 'Secure' || fail "会话 Cookie 缺少 Secure: $COOKIE"
echo "$COOKIE" | grep -qi 'SameSite=Lax' ||
  fail "会话 Cookie 缺少 SameSite=Lax（跨站点 IdP 回调将丢 Cookie）: $COOKIE"
pass "会话 Cookie 属性：HttpOnly + Secure + SameSite=Lax"

ACTION="$(grep -o '<form[^>]*id="kc-form-login"[^>]*>' "$HTML" | grep -o 'action="[^"]*"' | head -1 |
  sed -e 's/^action="//' -e 's/"$//' -e 's/&amp;/\&/g')"
[[ "$ACTION" == http* ]] || fail "未解析到 Keycloak 登录表单（页面头部：$(head -c 160 "$HTML")）"

CODE="$("${CURL[@]}" -c "$JAR" -b "$JAR" -L -o /dev/null -w '%{http_code}' \
  --data-urlencode 'username=bff-user' \
  --data-urlencode 'password=bff-pass' \
  --data-urlencode 'credentialId=' \
  "$ACTION")"
[[ "$CODE" == "200" ]] || fail "凭据提交后链路最终状态 $CODE"
SESSION="$("${CURL[@]}" -b "$JAR" "$BASE/api/session")"
echo "$SESSION" | grep -q '"logged_in":true' || fail "登录后会话状态异常: $SESSION"
pass "登录成功（授权码+PKCE+RS256/JWKS 验签通过），/api/session = $SESSION"

SESSLIST="$(curl -s --max-time 10 -H "X-Admin-Token: $BFF_ADMIN_TOKEN" "$ADMIN_API/sessions")"
echo "$SESSLIST" | grep -q '"provider":"keycloak"' ||
  fail "管理端会话列表未登记 keycloak 会话: $SESSLIST"
pass "管理端会话列表已登记 keycloak 会话（Redis session store）"

# ------------------------------------------------------------
step "4) Bearer 注入（上游收到真实 Keycloak access token）"
ECHO1="$("${CURL[@]}" -b "$JAR" "$BASE/api/echo")"
TOK1="$(printf '%s\n' "$ECHO1" | sed -n 's/^auth=Bearer \(.*\)$/\1/p')"
[[ -n "$TOK1" ]] || fail "上游未收到 Bearer 令牌: $ECHO1"
PAY1="$(jwt_payload "$TOK1")"
echo "$PAY1" | grep -q "\"iss\":\"$KC_ISSUER\"" || fail "access token issuer 不符: $PAY1"
echo "$PAY1" | grep -q '"azp":"bff"' || fail "access token azp 不符: $PAY1"
echo "$PAY1" | grep -q '"typ":"Bearer"' || fail "access token typ 不符: $PAY1"
pass "上游收到 Keycloak 签发的 RS256 access token（iss=$KC_ISSUER, azp=bff, typ=Bearer）"

# ------------------------------------------------------------
step "5) 会话存于 Redis：bff 容器重启后登录态不丢"
CID="$("${COMPOSE[@]}" ps -q bff)"
[[ -n "$CID" ]] || fail "未找到 bff 容器"
docker restart "$CID" >/dev/null
RESTART_OK=0
for _ in $(seq 1 60); do
  if [[ "$(http_code "$BASE/live")" == "200" ]]; then
    RESTART_OK=1
    break
  fi
  sleep 1
done
[[ $RESTART_OK == 1 ]] || fail "bff 重启后 /live 未恢复"
SESSION2="$("${CURL[@]}" -b "$JAR" "$BASE/api/session")"
echo "$SESSION2" | grep -q '"logged_in":true' || fail "容器重启后登录态丢失（Redis session 未生效）: $SESSION2"
pass "容器重启后 /api/session 仍 logged_in=true（会话不落进程内存）"

# ------------------------------------------------------------
step "6) 令牌刷新（SWR 后台刷新 + Keycloak REFRESH_TOKEN 事件）"
echo "  等待 access token 进入刷新窗口（realm AT 90s / skew 60s → 约 35s）..."
sleep 35
"${CURL[@]}" -b "$JAR" -o /dev/null "$BASE/api/echo" || true # 触发 SWR 后台刷新

TOK2=""
for _ in $(seq 1 30); do
  ECHO2="$("${CURL[@]}" -b "$JAR" "$BASE/api/echo" || true)"
  TOK2="$(printf '%s\n' "$ECHO2" | sed -n 's/^auth=Bearer \(.*\)$/\1/p')"
  [[ -n "$TOK2" && "$TOK2" != "$TOK1" ]] && break
  sleep 1
done
[[ -n "$TOK2" && "$TOK2" != "$TOK1" ]] || fail "access token 未在刷新窗口内轮换（上游仍收到旧 token）"

REFRESH_EVENTS="$(kc_event_count REFRESH_TOKEN)"
[[ "${REFRESH_EVENTS:-0}" -ge 1 ]] || fail "Keycloak 未观察到 REFRESH_TOKEN 事件"
pass "access token 已轮换（旧≠新），Keycloak REFRESH_TOKEN 事件 ×$REFRESH_EVENTS"

# ------------------------------------------------------------
step "7) RP-Initiated Logout（discovery end_session_endpoint + 回跳）"
CODE="$("${CURL[@]}" -c "$JAR" -b "$JAR" -L -o /dev/null -w '%{http_code}' "$BASE/logout")"
[[ "$CODE" == "200" ]] || fail "登出链路最终状态 $CODE"
SESSION3="$("${CURL[@]}" -b "$JAR" "$BASE/api/session")"
echo "$SESSION3" | grep -q '"logged_in":false' || fail "登出后会话状态异常: $SESSION3"

LOGOUT_EVENTS=0
for _ in $(seq 1 10); do # 事件写入有毫秒级延迟，轮询
  LOGOUT_EVENTS="$(kc_event_count LOGOUT)"
  [[ "${LOGOUT_EVENTS:-0}" -ge 1 ]] && break
  sleep 1
done
[[ "${LOGOUT_EVENTS:-0}" -ge 1 ]] || fail "Keycloak 未观察到 LOGOUT 事件（可能仍停在登出确认页）"
pass "登出成功：本地会话已清除，Keycloak LOGOUT 事件 ×$LOGOUT_EVENTS"

# ------------------------------------------------------------
step "8) 汇总"
LOGIN_EVENTS="$(kc_event_count LOGIN)"
echo ""
echo "🎉 Keycloak 真实 IdP 全链路 E2E 通过："
echo "   - 授权码+PKCE(S256) / RS256+JWKS 真实验签 / nonce / state 校验"
echo "   - redirect_uri 恒为 public_base_url 推导值（伪造 Host 未污染）"
echo "   - Bearer 注入：上游收到真实 Keycloak token"
echo "   - Redis 会话：bff 容器重启后登录态不丢"
echo "   - 令牌刷新：SWR 轮换 + Keycloak REFRESH_TOKEN 事件"
echo "   - RP-Initiated Logout：Keycloak LOGOUT 事件（LOGIN 事件 ×$LOGIN_EVENTS）"

if [[ "${KEEP_STACK:-0}" != "1" ]]; then
  "${COMPOSE[@]}" down >/dev/null 2>&1 || true
  echo ""
  echo "（栈已清理；保留现场：KEEP_STACK=1 bash $0）"
fi
