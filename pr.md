# Add oracle staking/slashing, governance upgrade proposals, and guardian multisig for critical admin ops

Closes #347, #348, #349, #350.

## Summary

Four related enhancements to protocol economic security and admin-key risk, implemented as fully opt-in / backward-compatible additions — no existing deployment, integration, or test behavior changes unless the new controls are explicitly configured.

- **#347 — Minimum stake for oracle registration** (`oracle-verifier`)
- **#348 — Oracle slashing for incorrect data** (`oracle-verifier`)
- **#349 — Governance-gated contract upgrades** (`governance-dao`)
- **#350 — Guardian multisig for critical admin operations** (all 5 contracts)

## #347 — Minimum stake requirement

Oracles previously registered with `add_oracle` with no economic backing, so there was no cost to submitting bad data.

- `set_stake_token(admin, token)` / `set_min_stake(admin, min_stake)` — admin configures the stake asset and required minimum (defaults to `0`, i.e. disabled).
- `stake(oracle, data_type, amount)` — an oracle self-deposits stake ahead of registration.
- `add_oracle` now rejects registration if the oracle's deposited stake for that `data_type` is below `min_stake`.
- `withdraw_stake(oracle, data_type)` — an oracle reclaims its stake, but only once it is not an active registration (never registered, or previously removed), so an active oracle can't pull its backing out from under a live registration.

Because `min_stake` defaults to `0`, this is fully backward compatible with the ~70 existing `add_oracle` call sites across `oracle-verifier` and `claims-processor` tests.

## #348 — Oracle slashing

- `slash_oracle(admin, oracle, data_type, amount, reason)` — burns (or redirects to an optional `set_slash_treasury` address) part of a misbehaving oracle's stake.
- Slashing is capped at the oracle's current stake, and if the remaining stake drops below `min_stake`, the oracle is automatically deactivated (removed from the active oracle list) so it can't keep submitting until it re-stakes and is re-registered.
- Emits `OracleSlashed` for off-chain monitoring/auditing.

## #349 — Governance-gated contract upgrades

Previously, authorizing a contract upgrade required a single admin key, with no DAO involvement — even though `governance-dao` already had a generic `create_proposal` → vote → finalize → timelock → `execute` pipeline capable of arbitrary `target::function(args)` calls.

- Added `ProposalKind::{Standard, Upgrade}` and a `kind` field on `Proposal`.
- `propose_upgrade(proposer, title, target, new_wasm_hash)` builds an `Upgrade`-kind proposal whose `execute()` calls `target::upgrade(dao_address, new_wasm_hash)` — so upgrading a contract that has the DAO configured as its admin now requires a full governance vote, not just an admin signature.
- Reuses the exact same threshold/deposit/vote/finalize/timelock lifecycle as `create_proposal`; only the proposal `kind` and the pre-built `target`/`function`/`args` differ.

## #350 — Guardian multisig for critical admin operations

Scoped deliberately to the two highest-blast-radius, irreversible admin actions rather than every admin-only setter (wrapping every routine parameter setter — `set_min_confidence`, `set_max_data_age`, etc. — in multisig would be impractical and break single-admin workflows for no real security benefit):

- **Contract upgrade** (`upgrade`) — arbitrary code execution, replaces the entire contract.
- **Admin transfer** (`propose_new_admin`), where that function exists — a single compromised admin key can otherwise unilaterally hijack the contract by proposing itself as `new_admin` and then accepting.

Added to all 5 contracts (`oracle-verifier`, `governance-dao`, `risk-pool`, `policy-engine`, `claims-processor` — the last has no admin-transfer function, so only `upgrade` is gated there):

- `set_guardians(admin, guardians, threshold)` configures an M-of-N guardian set. `threshold == 0` (the default) disables the requirement entirely — the admin can act alone, exactly as today.
- Once `threshold > 0`, `upgrade()` no longer applies the WASM change immediately — it stores a `PendingUpgrade` that requires `threshold` guardians to call `approve_upgrade` before the code is actually replaced. Same pattern for `propose_new_admin()` / `approve_admin_change()`.
- `cancel_pending_upgrade` lets the admin abort a pending upgrade before it collects enough approvals.

Because guardian multisig is opt-in (`threshold` defaults to `0`), none of the ~250 existing tests across the 5 contracts needed to change.

## Compatibility / risk

- All new checks are gated behind explicit admin configuration (`min_stake`, `stake_token`, `guardian_threshold` all default to `0`/unset/disabled).
- No existing function signatures changed.
- No storage migration needed — all new storage keys are additive.

## Test plan

- [x] `cargo build --workspace` — clean build, no new warnings.
- [x] `cargo test -p parashield-oracle-verifier` — 52/53 pass (1 pre-existing failure on `main`, unrelated to this change — verified via `git stash`).
- [x] `cargo test -p parashield-governance-dao` — 32/33 pass (1 pre-existing failure on `main`, same verification).
- [x] `cargo test -p parashield-risk-pool` — 54/60 pass (6 pre-existing failures on `main`, same verification).
- [x] `cargo test -p parashield-policy-engine` — 54/55 pass (1 pre-existing failure on `main`, same verification).
- [x] `cargo test -p parashield-claims-processor` — 49/49 pass.
- [ ] New feature coverage (stake/slash/guardian-approval flows) — no new unit tests added yet; recommend adding before merge if this isn't following up in a separate PR.
