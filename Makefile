NETWORK   ?= testnet
SOURCE    ?= deployer
WASM_DIR  := contracts/target/wasm32v1-none/release

.PHONY: build test test-verbose lint fmt clean deploy check-tools

## Build all contracts for release (WASM)
build:
	@echo "Building contracts..."
	cd contracts && cargo build --target wasm32v1-none --release --quiet
	@echo "Build complete. WASMs in $(WASM_DIR)/"

## Build with debug assertions (for detailed error messages during dev)
build-debug:
	cd contracts && cargo build --target wasm32v1-none --profile release-with-logs

## Run all contract tests
test:
	cd contracts && cargo test --quiet 2>&1 | tail -20

## Run tests with full output
test-verbose:
	cd contracts && cargo test -- --nocapture

## Run tests for a specific contract
test-oracle:
	cd contracts && cargo test -p parashield-oracle-verifier -- --nocapture

test-policy:
	cd contracts && cargo test -p parashield-policy-engine -- --nocapture

test-claims:
	cd contracts && cargo test -p parashield-claims-processor -- --nocapture

test-pool:
	cd contracts && cargo test -p parashield-risk-pool -- --nocapture

test-dao:
	cd contracts && cargo test -p parashield-governance-dao -- --nocapture

## Run Clippy linter
lint:
	cd contracts && cargo clippy --all-targets -- -D warnings

## Format Rust source
fmt:
	cd contracts && cargo fmt --all

## Check formatting without modifying files
fmt-check:
	cd contracts && cargo fmt --all -- --check

## Remove build artifacts
clean:
	cd contracts && cargo clean

## Deploy to testnet (requires stellar CLI and funded deployer key)
deploy:
	./scripts/deploy_testnet.sh

## Check required tools are installed
check-tools:
	@which stellar > /dev/null || (echo "Error: stellar CLI not found. Install from https://github.com/stellar/stellar-cli" && exit 1)
	@which cargo   > /dev/null || (echo "Error: cargo not found. Install Rust from https://rustup.rs" && exit 1)
	@rustup target list --installed | grep -q wasm32v1-none || (echo "Adding wasm32v1-none target..." && rustup target add wasm32v1-none)
	@echo "All required tools found."

## Print sizes of compiled WASMs
wasm-sizes: build
	@echo "Contract WASM sizes:"
	@ls -lh $(WASM_DIR)/*.wasm 2>/dev/null || echo "No WASMs built yet. Run 'make build' first."
