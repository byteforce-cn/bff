.PHONY: clean fmt lint test check bff-build ui-build build iam-build iam-run iam-clean

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
