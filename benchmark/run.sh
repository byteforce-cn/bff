#!/usr/bin/env bash
# ============================================================
# BFF Benchmark Runner
# 用法:
#   ./run.sh smoke        — 冒烟测试
#   ./run.sh baseline     — 基线测试
#   ./run.sh stress       — 压力测试
#   ./run.sh endurance    — 耐久测试
#   ./run.sh all          — 依次运行所有场景
# ============================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS_DIR="${SCRIPT_DIR}/results"
K6_BIN="${K6_BIN:-k6}"
BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"

# 颜色
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }

# 确保 k6 可用
check_k6() {
    if ! command -v "$K6_BIN" &> /dev/null; then
        error "k6 未安装，请先安装: https://grafana.com/docs/k6/latest/set-up/install-k6/"
        exit 1
    fi
    info "k6 version: $("$K6_BIN" version 2>&1 | head -1)"
}

# 确保结果目录存在
ensure_results_dir() {
    mkdir -p "$RESULTS_DIR"
}

# 检查 BFF 是否存活
check_bff_alive() {
    info "检查 BFF 存活状态: ${BASE_URL}/live ..."
    if curl -s -o /dev/null -w "%{http_code}" --connect-timeout 3 "${BASE_URL}/live" 2>/dev/null | grep -q "200"; then
        info "BFF 存活，继续测试"
    else
        warn "BFF ${BASE_URL}/live 无响应，继续测试（可能失败）"
    fi
}

# 运行单个场景
run_scenario() {
    local scenario="$1"
    local k6_script="${SCRIPT_DIR}/k6-load-test.js"

    echo ""
    echo "============================================"
    info "场景: ${scenario}"
    echo "============================================"

    "$K6_BIN" run \
        --env BASE_URL="${BASE_URL}" \
        --env SCENARIO="${scenario}" \
        --env RESULTS_DIR="${RESULTS_DIR}" \
        "$k6_script"

    local exit_code=$?
    if [ $exit_code -eq 0 ]; then
        info "场景 ${scenario} 完成 ✓"
    else
        error "场景 ${scenario} 失败 (exit=${exit_code})"
    fi
    return $exit_code
}

# 主入口
main() {
    local scenario="${1:-smoke}"

    check_k6
    ensure_results_dir
    check_bff_alive

    case "$scenario" in
        smoke|baseline|stress|endurance)
            run_scenario "$scenario"
            ;;
        all)
            local failed=0
            for s in smoke baseline stress endurance; do
                if ! run_scenario "$s"; then
                    failed=$((failed + 1))
                fi
                sleep 2  # 间隔避免端口残留
            done
            echo ""
            if [ $failed -eq 0 ]; then
                info "所有场景通过 ✓"
            else
                error "${failed} 个场景失败"
                exit 1
            fi
            ;;
        *)
            echo "用法: $0 {smoke|baseline|stress|endurance|all}"
            exit 1
            ;;
    esac
}

main "$@"
