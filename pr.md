# Fix storage TTL expiry on oracle readings, proposals, and votes: #184, #185, #186

## Summary

Fixes three storage-TTL bugs where persistent Soroban storage entries had no
`extend_ttl` call, meaning they could expire (and become unreadable) while
still logically "in use" by the protocol:

- **#184 (oracle-verifier)**: `submit_data` (and the `batch_submit_data` /
  `submit_data_batch` variants) never extended the TTL of the `DataPoints`
  entry. A reading submitted once for a (data_type, key) that never receives
  another submission could expire before `claims-processor` calls
  `verify_trigger`, causing a spurious rejection on a legitimate claim. Fixed
  by extending each `DataPoints` entry's TTL to a 120-day retention window
  (clamped to the network's max TTL) on every write.

- **#185 (governance-dao)**: `Proposal`, `VoteRecord`, and `LockedBalance`
  entries were never TTL-extended. A proposal with a long voting period +
  timelock could have its record — and any votes/locked tokens — expire
  before `finalize`/`execute`/`withdraw_tokens` ran, losing the audit trail.
  Fixed by extending each entry's TTL to cover
  `voting_period + finalize_delay + proposal_timelock + a 30-day buffer`
  (clamped to the network's max TTL) whenever the entry is written.

- **#186 (all 5 contracts)**: no test exercised TTL expiry/extension
  behavior. Added one test per contract that advances the ledger sequence
  number past the default 4096-ledger `min_persistent_entry_ttl` and asserts
  the relevant entries are still readable. Since the other three contracts
  (`claims-processor`, `risk-pool`, `policy-engine`) had no `extend_ttl`
  calls either, the same class of bug is fixed there too, on their
  long-lived persistent entries (`Claim`/`PolicyClaim`,
  `LpPosition`/`Lock`/`LpAddress`/`AdminWithdrawalRequest`,
  `Policy`/`Product`/`ProductKey`/`UserPolicies`).

## Incidental fixes

The workspace didn't compile/test at all before this change (unrelated to
TTL), which had to be fixed to add and run the new tests:

- `claims-processor`: removed a duplicate `require_admin` function definition
  (hard compile error).
- `policy-engine`: added the missing `use alloc::string::ToString;` import
  needed by `Symbol::to_string()` in `create_product` (compile error under
  default features).
- `claims-processor/src/test_advanced.rs`: fixed a bad `*result` deref and
  three missing `&` on `process_claim(&keeper, claim_id)` calls (compile
  errors in the test target).

## Known pre-existing issues (out of scope, not touched)

Running the full suite surfaced several pre-existing, unrelated bugs, never
previously caught because the crates didn't compile:

- `oracle-verifier`: `add_oracle` double-pushes the oracle address onto
  `OracleList`; `test_update_oracle_weight_changes_aggregation` sets the
  ledger clock to timestamp `1` but submits data timestamped in 2025.
- `governance-dao`: `test_finalize_refunds_deposit_locked_at_creation_not_live_config`
  asserts a voter's full balance is restored at `finalize()` without ever
  calling `withdraw_tokens()` to release their locked voting weight.
- `policy-engine`: `sequential_create_product_ids_are_unique_and_monotone`
  uses a 2-character `oracle_key`, which fails the `MIN_LEN = 3` validation
  added for a previous issue.
- `claims-processor`: several integration tests fund `pool_id` with USDC
  instead of `policy_id`, so `pay_claim`'s payout transfer (which spends the
  Policy Engine's own balance) fails with `InsufficientPool`; some
  `test_integration.rs` tests never call `lock_for_policy`, so
  `release_for_claim` fails with `LockNotFound`.

These are fund-flow/test-fixture bugs unrelated to storage TTL and are left
untouched here.

## Test plan

- [x] `cargo check --workspace` — no errors
- [x] `cargo test -p parashield-oracle-verifier` — new TTL test passes
- [x] `cargo test -p parashield-governance-dao` — new TTL test passes
- [x] `cargo test -p parashield-risk-pool` — new TTL test passes
- [x] `cargo test -p parashield-policy-engine` — new TTL test passes
- [x] `cargo test -p parashield-claims-processor` — new TTL test passes
  (pre-existing unrelated failures noted above still fail, as expected)
