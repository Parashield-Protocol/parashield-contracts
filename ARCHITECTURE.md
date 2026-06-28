# Parashield Protocol Architecture

## Overview

Parashield is a decentralised parametric insurance protocol built on Stellar Soroban.
Unlike traditional insurance, claims are settled automatically by smart contracts
when a real-world trigger condition is confirmed by an oracle network.
No adjuster. No form. No delay.

## Contract Map

```
┌──────────────────────────────────────────────────────────┐
│                     User / DApp                          │
└────────────────────┬──────────────────────────────────────┘
                     │  buy_policy / submit_claim
                     ▼
┌──────────────────────────────────────────────────────────┐
│            Policy Engine (policy-engine)                 │
│  - Products catalogue (admin-defined)                    │
│  - Policy lifecycle: Active → Claimed / Expired          │
│  - Holds USDC escrow until payout or expiry              │
└──────┬──────────────────────────────────┬────────────────┘
       │ get_policy / pay_claim /         │ get_contract_balance
       │ expire_policy                    │
       ▼                                  ▼
┌─────────────────────┐         ┌─────────────────────────┐
│  Claims Processor   │         │     Risk Pool           │
│  (claims-processor) │         │     (risk-pool)         │
│                     │         │  - LP deposits USDC     │
│  - Evaluates oracle │         │  - Pool tokens (shares) │
│  - Calls pay_claim  │         │  - Yield from premiums  │
│    or expire_policy │         │  - Locks coverage cap.  │
└──────┬──────────────┘         └─────────────────────────┘
       │ verify_trigger
       ▼
┌──────────────────────────────────────────────────────────┐
│            Oracle Verifier (oracle-verifier)             │
│  - Multiple oracles submit signed observations           │
│  - Confidence-weighted median aggregation                │
│  - verify_trigger: returns bool for trigger condition    │
└──────────────────────────────────────────────────────────┘

         Admin / Token Holders
                  │
                  ▼
       ┌────────────────────┐
       │  Governance DAO    │
       │  (governance-dao)  │
       │  - Proposals       │
       │  - Token voting    │
       │  - Protocol params │
       └────────────────────┘
```

## Data Flow: Parametric Payout

```
1. Admin creates InsuranceProduct (oracle key, threshold, comparison)
2. User calls buy_policy(product_id, coverage_amount, duration_days, oracle_key)
   - Premium = coverage * premium_rate_bps / 10_000
   - USDC premium transferred from user to Policy Engine
   - Policy record created with status = Active
3. Oracle(s) submit data via oracle-verifier.submit_data() periodically
4. Keeper calls claims-processor.auto_process(policy_id)
   - Claims Processor calls oracle-verifier.verify_trigger(condition)
   - If trigger met:  Policy Engine.pay_claim() → USDC → policyholder
   - If trigger not met AND policy expired: Policy Engine.expire_policy()
```

## Fixed-Point Math

All monetary values use 7-decimal fixed point matching Stellar's native precision:

| Display value | On-chain representation |
|---------------|------------------------|
| 1 USDC        | 10_000_000             |
| 50.5 mm rain  | 505_000_000            |
| 120 min delay | 1_200_000_000          |

## Oracle Key Format

Oracle keys follow a structured naming convention (max 9 chars = Soroban Symbol):

| Data type   | Key format                    | Example     |
|-------------|-------------------------------|-------------|
| Rainfall    | `{loc}{yyyymm}`               | `kis2606`   |
| Temperature | `tmp{loc}{mm}`                | `tmpkis06`  |
| Flight      | `fl{flight}{dd}`              | `flkq10015` |
| Wind speed  | `wnd{loc}{mm}`                | `wndmom06`  |
| DeFi event  | `defi{proto}`                 | `defiave`   |

## Risk Pool Economics (v2)

```
Premium flow:
  80% → Risk Pool (LP yield)
  10% → Protocol Treasury (governance-controlled)
  10% → Backstop Fund (solvency reserve)

Utilization rate = total_active_coverage / total_deposited_liquidity

Target APY ranges:
  Low-risk pools (crop, flight): 8–15%
  Medium-risk (disaster):        15–25%
  High-risk (DeFi exploit):      25–40%
```

**Pool size limits.** Each pool caps cumulative deposits at
`MAX_TOTAL_DEPOSITED = 10^15` stroops (7-decimal USDC). Deposits that would push
`total_deposited` past this bound are rejected with `PoolCapExceeded`. The cap
keeps `total_shares` within safe `i128` range and prevents per-share value from
becoming infinitesimal under unbounded deposit growth.

## Governance DAO (v2)

SHIELD token holders govern protocol parameters:

- Add / remove insurance products
- Adjust premium rates and trigger thresholds
- Register / deregister oracle sources
- Allocate protocol treasury funds
- Emergency pause individual contracts

**Proposal lifecycle:** Draft → Active (7-day voting) → Passed (≥10% quorum, simple majority) → Executed (2-day timelock)

## Security Notes

- Admin keys should transition to Governance DAO after protocol launch
- Oracle submissions are bounded by registered oracle set (not open)
- Policy Engine holds USDC in escrow: no admin withdrawal function
- Claims Processor is the only address authorized to call `pay_claim` / `expire_policy`
- All monetary arithmetic uses checked arithmetic (Soroban default with overflow-checks = true)
