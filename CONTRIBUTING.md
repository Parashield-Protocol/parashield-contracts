# Contributing to Parashield Contracts

## Setup

```bash
# Install Rust + Soroban target
rustup target add wasm32v1-none

# Build
make build

# Test
make test

# Lint
make lint
```

## Conventions

- All storage keys use `#[contracttype]` enums.
- Monetary values use 7-decimal fixed-point `i128` (1 USDC = 10_000_000).
- All public functions that modify state require `caller.require_auth()`.
- Errors use `#[contracterror]` with explicit `#[repr(u32)]` codes.
- Tests live in `src/test.rs` (unit) and `src/test_integration.rs` (cross-contract).

## PR Checklist

- [ ] `make test` passes
- [ ] `make lint` passes with zero warnings
- [ ] New public functions have a short doc comment explaining the invariants
- [ ] Error codes do not collide with existing ones
- [ ] Persistent storage keys are documented in the StorageKey enum comment

## Adding a New Contract

1. `cargo new --lib contracts/<name>`
2. Add to `contracts/Cargo.toml` `[workspace.members]`
3. Set `crate-type = ["cdylib", "rlib"]` in the crate's `Cargo.toml`
4. Add a `testutils` feature that enables `soroban-sdk/testutils`
5. Write at least 5 unit tests covering init, happy path, and error paths
6. Update `ARCHITECTURE.md` with the new contract's role

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(oracle-verifier): add batch_submit_data
fix(risk-pool): guard against zero deposit edge case
test(claims-processor): add dispute resolution tests
chore: update soroban-sdk to 22.1
```
