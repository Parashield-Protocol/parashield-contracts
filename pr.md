# Fix missing storage TTL extension on claim, lock, and policy entries: #181, #182, #183

## Summary

Fixes three storage-TTL bugs where persistent Soroban entries could expire from
storage (default ~20 days of inactivity) well before the business object they
represent (a pending claim, a locked capital position, or a policy) reaches
its natural end of life:

- **claims-processor (#183)**: `submit_claim`, `auto_process`, and
  `evaluate_and_settle` (the write path shared by `process_claim`,
  `auto_process`, and `batch_auto_process`) now call `extend_ttl` on the
  `Claim` and `PolicyClaim` persistent entries whenever they are written, so a
  pending claim can no longer be evicted from storage before a keeper
  processes it.
- **risk-pool (#182)**: `lock_for_policy` (a.k.a. `lock_capital`) and
  `release_for_claim` now call `extend_ttl` on the `Lock` entry. Also applied
  the same fix to `release_for_expiry`, which writes the identical `Lock`
  entry and was subject to the same eviction risk. A capital lock backing a
  long-dated policy will no longer silently expire from storage before the
  policy matures.
- **policy-engine (#181)**: `create_product` and `buy_policy` now call
  `extend_ttl` on the `Product`/`ProductKey` and `Policy`/`UserPolicies`
  persistent entries respectively, so a policy purchased for up to the
  product's `max_duration_days` no longer risks losing its storage entry
  (and any associated claims data) to rent expiry.

Each contract extends entries out to ~1 year (`6_312_000` ledgers at ~5s/ledger)
once they drop below a ~30-day (`518_400` ledger) threshold.



## Test plan

- [x] `cargo build --workspace` succeeds
- [ ] `cargo test --workspace` passes


## Not changed

- **oracle-verifier (#180)**: investigated and found already fixed —
  `add_oracle` and `update_oracle_weight` both reject `weight == 0` via
  `Error::InvalidWeight`, and this is covered by
  `test_cannot_update_oracle_to_invalid_weight` in
  `contracts/oracle-verifier/src/test.rs`. No code change needed.
