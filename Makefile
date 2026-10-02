.PHONY: clean fmt lint test check bff-build ui-build build iam-build iam-run iam-clean audit bench coverage https-e2e mock-idp snapshot gitleaks

clean:
	cargo clean
	cd iam && mvn -q clean
	rm -rf admin-ui/dist admin-ui/node_modules

fmt:
	cargo fmt --all

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --all-features

check:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --all-features

# 依赖审计（需 cargo-audit；例外清单见 .cargo/audit.toml）
audit:
	cargo audit

# 基准压测（需 Docker 与运行中的 BFF；场景：smoke|baseline|capacity|stress|endurance）
# 认证路径需传 COOKIE（获取方式见 benchmark/README.md）
bench:
	cd benchmark && docker run --rm -i --network host -v "$$PWD":/bench -w /bench \
		grafana/k6 run --env SCENARIO=$${SCENARIO:-smoke} $${COOKIE:+--env COOKIE=$$COOKIE} k6-load-test.js

# 覆盖率测量（需 cargo-llvm-cov；CI 门禁见 .github/workflows/ci.yml）
coverage:
	BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo llvm-cov --all-features --summary-only

# HTTPS + LB 全链路 E2E（需 Docker；Mock IdP 见 mock-idp）
https-e2e:
	bash deploy/https/gen-certs.sh
	bash deploy/https/e2e.sh

# 本地 Mock OIDC Provider（示例二进制，仅验收演示）
mock-idp:
	MOCK_IDP_ISSUER=http://host.docker.internal:9090 cargo run --release --example mock_idp

# BFF release 构建：管理端未构建时，build.rs 会在 admin-ui/dist 生成占位页，保证编译通过
bff-build:
	cargo build --release

ui-build:
	cd admin-ui && pnpm install && pnpm build

build: ui-build bff-build

# IAM — Spring Authorization Server 测试用 OIDC Provider (port 9090)
iam-build:
	cd iam && mvn -q package -DskipTests

iam-run:
	cd iam && mvn -q spring-boot:run

iam-clean:
	cd iam && mvn -q clean

# 源码快照（发布/分发用）：仅含 tracked 文件（自动排除 tmp/、密钥与构建产物）
snapshot:
	@mkdir -p dist
	git archive --format=tar.gz --prefix=bff-$(shell git describe --tags --always --dirty)/ -o dist/bff-$(shell git describe --tags --always --dirty).tar.gz HEAD
	@ls -lh dist/bff-*.tar.gz

# 凭据/密钥扫描（需本机安装 gitleaks；例外与理由见 .gitleaks.toml）
gitleaks:
	gitleaks detect --source . --config .gitleaks.toml --redact --verbose
