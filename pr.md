# Close test-coverage and doc gaps: #164, #165, #166, #167

## Summary

- **#164 (policy-engine)**: Added `cancel_policy_refund_matches_hand_calculated_value_at_known_elapsed`, which cancels a policy at a known elapsed duration (10 of 30 days) and asserts the exact `premium_paid` and refund amounts against hand-calculated values, catching off-by-one/integer-division bugs the existing approximate midpoint test would miss.
- **#165 (governance-dao)**: Added `finalize_with_exactly_tied_votes_fails`, which drives `votes_for == votes_against` via two equal-weight voters on opposite sides and asserts `finalize()` marks the proposal `Failed` (50% for-share is below the 51% majority threshold).
- **#166 (risk-pool)**: Added `lock_for_policy_on_empty_pool_fails_undercollateralized`, which calls `lock_for_policy` on a pool with zero deposits and asserts it rejects with `Undercollateralized` (#11).
- **#167 (governance-dao)**: Added rustdoc to all 14 previously-undocumented public functions (`initialize`, `create_proposal`, `vote`, `withdraw_tokens`, `finalize`, `execute`, `cancel`, `get_proposal`, `get_vote`, `get_config`, `get_admin`, `proposal_count`, `get_version`, `update_config`), explaining the token-locking mechanism during voting, the quorum/majority math in `finalize()`, and the timelock behavior of `execute()`.

## Incidental fix

While compiling to verify the new tests, found that `risk-pool`'s `Error` enum assigned discriminant `17` to both `InsufficientShares` and `DepositTooSmall`, which fails to compile under `#[repr(u32)]`. This silently blocked the entire `risk-pool` crate (including its full existing test suite) from ever building. Fixed by giving `InsufficientShares` its own discriminant (`18`); `DepositTooSmall` keeps `17` to match existing tests asserting `Error(Contract, #17)`.

## Note on pre-existing failures (not in scope, left as-is)

Once `risk-pool` could compile, two pre-existing failures surfaced that are unrelated to the four issues above and were not introduced by this change (verified against unmodified `main`):
- `risk-pool::test::withdraw_uses_available_liquidity_after_locks` and its duplicate `_2`
- `governance-dao::test::test_finalize_refunds_deposit_locked_at_creation_not_live_config`

These look like real bugs (e.g. `vote()` locks a voter's entire token balance, but `finalize()` only refunds the `create_proposal` deposit, not the voting lock) but are out of scope for this PR and are flagged here for a separate fix.

## Test plan

- [x] `cargo test -p parashield-policy-engine` — all pass
- [x] `cargo test -p parashield-governance-dao` — all pass except the pre-existing, unrelated `test_finalize_refunds_deposit_locked_at_creation_not_live_config`
- [x] `cargo test -p parashield-risk-pool` — all pass except the pre-existing, unrelated `withdraw_uses_available_liquidity_after_locks[_2]`
