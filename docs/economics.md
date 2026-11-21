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

### Canonical Smart Contract Formula (365-Day Annualized Base)

The smart contract implements an annualized premium calculation using a 365-day base and integer ceiling division to ensure the risk pool never undercollects:

$$\text{premium} = \left\lceil \frac{\text{coverage\_amount} \times \text{premium\_rate\_bps} \times \text{duration\_days}}{365 \times 10{,}000} \right\rceil$$

In Soroban contract integer arithmetic:
```rust
let numerator = coverage_amount
    .checked_mul(product.premium_rate_bps as i128)
    .and_then(|v| v.checked_mul(duration_days as i128))
    .unwrap_or_else(|| panic_with_error!(env, Error::CoverageOutOfRange));
let divisor: i128 = 365 * 10_000;
// Ceiling integer division: (numerator + divisor - 1) / divisor
let required_premium = numerator
    .checked_add(divisor - 1)
    .and_then(|v| v.checked_div(divisor))
    .unwrap_or_else(|| panic_with_error!(env, Error::CoverageOutOfRange));
```

### Protocol Discrepancy & Resolution (Issue #808)

- **Legacy Backend Formula (30-Day Monthly Base)**:
  Earlier versions of the backend off-chain quoting services calculated premiums using a 30-day monthly base:
  $$\text{premium}_{\text{backend}} = \left\lceil \frac{\text{coverage\_amount} \times \text{premium\_rate\_bps} \times \text{duration\_days}}{10{,}000 \times 30} \right\rceil$$
- **Discrepancy**:
  Because $\frac{365}{30} \approx 12.167$, this created an approximate $12\times$ discrepancy where the backend quoted ~12 times higher premiums than the smart contract enforced for the same duration.
- **Protocol Standard**:
  The smart contract's 365-day annualized formula is the authoritative protocol standard. All off-chain backend quoting engines, APIs, and client-side calculators must use the 365-day annualized formula with ceiling integer division matching the on-chain contract.

### Worked Example

- **Coverage**: 1,000 USDC (`1_000_000_000` stroops / 7 decimals)
- **Rate**: 5.0% (`500` bps)
- **Duration**: 30 days
- **Numerator**: $1{,}000{,}000{,}000 \times 500 \times 30 = 15{,}000{,}000{,}000{,}000$
- **Divisor**: $365 \times 10{,}000 = 3{,}650{,}000$
- **Contract Premium**: $\lceil 15{,}000{,}000{,}000{,}000 / 3{,}650{,}000 \rceil = 4{,}109{,}590$ stroops ($\approx 4.109590$ USDC)
- **Legacy Backend (Incorrect)**: $\lceil 15{,}000{,}000{,}000{,}000 / 300{,}000 \rceil = 50{,}000{,}000$ stroops ($50.0$ USDC, ~12.17x higher)

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

## LP Share Mechanics

- First deposit: 1 share = 1 USDC (no dilution risk at launch)
- Subsequent deposits: `new_shares = deposit_amount × total_shares / total_deposited`
- Shares track proportional ownership of the pool's total USDC balance
- Share value decreases after a claim payout (loss socialized across LPs)
- Share value increases as premium yield accumulates

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

## Risk Categories

| Category | Premium Rate | Target APY | Max Coverage |
|----------|-------------|------------|--------------|
| Crop     | 3.0%        | 12–18%     | 100,000 USDC |
| Flight   | 1.5%        | 8–12%      | 10,000 USDC  |
| DeFi     | 5.0%        | 20–40%     | 1,000,000 USDC |
| Disaster | 2.0%        | 10–15%     | 500,000 USDC |
