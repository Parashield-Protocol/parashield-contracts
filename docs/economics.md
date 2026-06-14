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

## Risk Categories

| Category | Premium Rate | Target APY | Max Coverage |
|----------|-------------|------------|--------------|
| Crop     | 3.0%        | 12–18%     | 100,000 USDC |
| Flight   | 1.5%        | 8–12%      | 10,000 USDC  |
| DeFi     | 5.0%        | 20–40%     | 1,000,000 USDC |
| Disaster | 2.0%        | 10–15%     | 500,000 USDC |
