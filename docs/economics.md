# Parashield Protocol Economics

## Premium Flow

```
Policy buyer pays premium
        │
        ▼
  PolicyEngine holds USDC
        │
  ClaimsProcessor calls receive_premium()
        │
  RiskPool splits:
    ├── 80% → LP yield accumulated (distributed to LPs pro-rata on claim_yield())
    ├── 10% → Protocol treasury
    └── 10% → Backstop fund (reserved for catastrophic loss events)
```

## Premium Rate

Premium = `coverage_amount × premium_rate_bps / 10_000`

Example: 1,000 USDC coverage at 3% rate = 30 USDC premium.

## Policy Trigger Threshold Rules (Issue #773)

Insurance policies use parametric or threshold-based conditions tied to verified oracle reports.
- **Allowed Range:** Trigger thresholds must be strictly positive and capped at the protocol-wide 7-decimal fixed-point ceiling:
  ```
  0 < trigger_threshold <= 1_000_000_000_000_000_000_000 (10^21)
  ```
- **Boundary Rationale:**
  - *Non-positive values (`threshold <= 0`):* Forbidden. A zero or negative threshold in a threshold or parametric comparison (e.g., `value <= threshold` or `value >= threshold`) could cause policies to trigger unconditionally or never trigger.
  - *Overflow bound (`threshold > 10^21`):* Forbidden. Prevents astronomical thresholds that exceed oracle metric capabilities or risk numeric overflow in settlement math.
- **Enforcement Points:**
  - `create_product`: Validates that `params.trigger_threshold` satisfies `0 < threshold <= 10^21`.
  - `buy_policy_inner`: Defense-in-depth re-validation guaranteeing active product threshold remains within bounds.
  - `buy_policy_with_threshold`: Allows policyholders to specify their expected threshold, rejecting with `Error::InvalidTriggerThreshold` (code #14) if it diverges from product configuration or protocol bounds.

## Claim Settlement

```
Trigger condition met (oracle confirms)
        │
  ClaimsProcessor calls pay_claim() on PolicyEngine
        │
  PolicyEngine transfers coverage_amount USDC → policyholder
        │
  RiskPool releases capital lock for this policy
```

### Multi-Recipient Payout & Escrow Dust Policy (Issue #772)

When insurance claims are settled across multiple recipients (e.g. syndicated policies, co-insured assets, or designated beneficiaries):
- **Base Allocation:** Payouts are computed using integer division: `base_amount = total_payout / count`.
- **Dust Remainder Handling:** Any remainder `remainder = total_payout % count` is allocated in full to the last recipient:
  - Recipient `0 .. count - 1`: `base_amount`
  - Last recipient (`count - 1`): `base_amount + remainder`
- **Zero Dust Guarantee:** The sum of recipient disbursements exactly equals the total approved claim payout:
  ```
  sum(disbursements) == total_payout
  escrow_dust == 0
  ```
  No unaccounted dust accumulates in protocol escrow.

## LP Share Mechanics

- First deposit: 1 share = 1 USDC (no dilution risk at launch)
- Subsequent deposits: `new_shares = (deposit_amount × total_shares + total_deposited / 2) / total_deposited`
- Shares track proportional ownership of the pool's total USDC balance
- Share value decreases after a claim payout (loss socialized across LPs)
- Share value increases as premium yield accumulates

### Share Price & Rounding Policy (Issue #770)

- **Share Price Metric:** Per-share price is queried via `get_share_price()` and scaled by `SHARE_PRICE_SCALE = 1_000_000_000` (1e9).
  ```
  share_price = (total_deposited × 1e9 × SHARE_PRICE_SCALE + total_shares / 2) / total_shares
  ```
- **Rounding Consistency:** To avoid truncation value leak on small deposits (where integer division truncates fractions toward zero), calculations apply consistent **round-half-up** arithmetic (`(numerator + denominator / 2) / denominator`).
- **Precision Preservation:** Minimum deposit amounts (`MIN_DEPOSIT = 1_000_000` stroops) receive full proportional share allocation without precision truncation, preventing economic dilution.

## Utilization Rate

```
utilization_bps = total_locked × 10_000 / total_deposited
```

Target utilization: 60-80% (higher = more yield, higher tail risk).

## Governance Token (SHIELD)

- Used for voting weight in GovernanceDAO
- Holding SHIELD ≠ LP position; governance and liquidity provision are separate
- Proposal threshold: 10,000 SHIELD minimum to create a proposal
- Quorum: 10% of total SHIELD supply must vote
- Majority: 51% of cast votes must be FOR

### Proposal deposit refunds

The proposer locks the configured threshold at creation. Failed proposals and
admin-cancelled proposals refund the full locked amount. A passed proposal
keeps its deposit escrowed through finalization until execution. The target may
return a `u32` completion value in basis points (`0`–`10,000`); the DAO refunds
that fraction of the original deposit and retains the rest. Existing targets
that return unit or another non-`u32` value are treated as fully executed for
backward compatibility. A target invocation that fails atomically produces no
refund. Passed proposals that expire without execution retain the deposit.

### Proposal Hash Verification on Execution (Issue #771)

To prevent payload tampering between voting, timelock queueing, and final execution:
- **Hash Commitment at Creation:** When a proposal is created (standard or upgrade), a SHA-256 hash of its complete payload `(target, function, args)` is computed via `env.crypto().sha256(&payload_bytes)` and persistently stored under `StorageKey::ProposalHash(proposal_id)`.
- **Integrity Verification on Execution:** Prior to invoking the proposal target or applying an upgrade, `GovernanceDAO::execute` re-computes the hash of the payload and asserts strict equality with the stored hash.
- **Tampering Defense:** If any component of the payload (`target`, `function`, or `args`) has been tampered with or modified, execution aborts immediately with `Error::HashMismatch` (code #9), guaranteeing end-to-end payload immutability.

## Risk Categories

| Category | Premium Rate | Target APY | Max Coverage |
|----------|-------------|------------|--------------|
| Crop     | 3.0%        | 12–18%     | 100,000 USDC |
| Flight   | 1.5%        | 8–12%      | 10,000 USDC  |
| DeFi     | 5.0%        | 20–40%     | 1,000,000 USDC |
| Disaster | 2.0%        | 10–15%     | 500,000 USDC |
