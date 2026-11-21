# Contract security controls

## Policy oracle changes

Oracle rotation is a three-step operation. The admin calls
`propose_oracle_change`, the independently configured oracle guardian calls
`approve_oracle_change`, and the admin calls `execute_oracle_change` after the
48-hour review period. A pending proposal cannot be replaced, an unapproved
proposal cannot execute, and changing the guardian does not bypass the delay.

## Claims processor guardian multisig

The admin bootstraps the initial guardian set with `set_guardians`. Each
non-empty set must have a threshold from 1 through the set size, and duplicate
guardian addresses are rejected. After bootstrap, `set_guardians` only proposes
a replacement; the currently configured threshold must approve it through
`approve_guardian_config` before it takes effect. An active guardian set cannot
be cleared or disabled by setting its threshold to zero. The initial
admin-controlled bootstrap is the only configuration change that does not
require approval from an existing set. `guardian_config_proposed` and
`guardian_config_approved` report the change and approval progress;
`guardians_updated` is emitted only when the change takes effect.

## Policy expiration event

When the authorized claims processor expires a policy, PolicyEngine emits the
`policy_expired` event with a `PolicyExpired` payload containing `policy_id`
(`u128`) and `timestamp` (`u64`, the current ledger timestamp).

## Claim dispute windows

The claims processor defaults to a seven-day dispute window. The admin may set
the window from one hour (`MIN_DISPUTE_WINDOW`) through 30 days
(`MAX_DISPUTE_WINDOW`), inclusive. Zero is invalid because it would remove a
claimant's opportunity to dispute a rejected or partially paid claim. Pending
claims remain disputable because no decision timestamp exists yet.

## LP NFT ID space

LP NFT IDs are sequential nonzero `u64` values. Minting the final representable
ID fails with `Error::Overflow`; the counter never wraps to zero or overwrites
an earlier NFT. In practice this makes exhaustion theoretical while keeping
the terminal behavior explicit and safe.

## Fraud detection

The claims processor can run an optional, additive rule-based fraud detector
on every `submit_claim` / `batch_submit_claims` call. It is off until the
admin calls `set_fraud_config` with a `FraudConfig`. There is no call to
remove the configuration afterward, so once set, every submission is scored;
setting `fraud_threshold_score` to `0` effectively disables flagging again,
since a claim can only be treated as fraudulent when the threshold is
nonzero.

Each submission is scored against up to three independent rules. A rule only
contributes if its own fields are nonzero, so any subset can be disabled by
zeroing its score or window:

- **Rate (`rate_score`)** — fires when the claimant's previous submission was
  less than `rate_window_secs` ago. Never fires on a claimant's first-ever
  claim, since there is no prior submission to compare against.
- **Burst (`burst_score`)** — fires when this submission would be the
  `burst_count`-th inside the current `burst_window_secs` bucket. The bucket
  resets once `burst_window_secs` elapses since it started; otherwise each
  submission inside the window extends the running count.
- **Coverage anomaly (`coverage_score`)** — fires when `coverage_amount`
  exceeds `coverage_anomaly_multiplier` times the claimant's highest prior
  `coverage_amount`. Never fires on a first-ever claim, since there is no
  prior coverage to compare against.

Rules are additive: every rule that fires adds its score, so a submission
that is both a rapid resubmission *and* a coverage anomaly accumulates both
`rate_score` and `coverage_score`. The claim is treated as fraudulent when
the summed score is at or above `fraud_threshold_score` (capped at 100 by
`set_fraud_config`).

`claimant_history` (last submission time, burst bucket, highest coverage
seen, total submissions) is updated *before* the fraud check runs on every
submission attempt, blocked or not — otherwise a claimant could spam
blocked submissions without ever moving their own rate/burst counters.

### `Block` vs `FlagOnly`

`FraudConfig.mode` decides what happens once the threshold is met:

- **`Block`** (default, safest) — the call panics with `FraudSuspected`
  before any claim state is written. The claimant's history still reflects
  the attempt; nothing else about the world changes.
- **`FlagOnly`** — the claim is admitted normally into the pending queue.
  A `FraudRecord` is persisted and a `FraudFlagged` event is emitted so an
  admin can review and act on the claim later, without blocking a possibly
  legitimate claimant.

### Tuning for different risk profiles

- A low-friction product (fast, low-value payouts) favors a higher
  `fraud_threshold_score` and/or `FlagOnly`, so occasional false positives
  from legitimate repeat claimants don't block them outright.
- A high-value product favors a lower threshold and `Block`, accepting more
  manual-review friction in exchange for stopping a suspected-fraudulent
  claim before any processing state exists for it.
- Setting any rule's score or window to `0` disables that rule without
  affecting the others, so a profile can run with only coverage-anomaly
  detection, only rate limiting, or any other combination.

## Governance delegation

Delegation is deliberately one hop. Self-delegation, delegation to an account
that has delegated onward, and delegation by an account that already receives
delegations are rejected. A proposed edge that points back to its delegator is
identified specifically as `CircularDelegation`, so no cycle can enter the
stored delegation graph or be traversed while calculating voting weight.
