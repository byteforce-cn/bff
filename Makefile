.PHONY: clean fmt lint test check bff-build ui-build build iam-build iam-run iam-clean audit https-e2e mock-idp

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

# E4：依赖审计（需 cargo-audit；例外清单见 .cargo/audit.toml）
audit:
	cargo audit

# HTTPS + LB 全链路 E2E（需 Docker；Mock IdP 见 mock-idp）
https-e2e:
	bash deploy/https/gen-certs.sh
	bash deploy/https/e2e.sh

# 本地 Mock OIDC Provider（示例二进制，仅验收演示）
mock-idp:
	MOCK_IDP_ISSUER=http://host.docker.internal:9090 cargo run --release --example mock_idp

bff-build:
	@mkdir -p admin-ui/dist
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
