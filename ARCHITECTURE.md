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
- Oracle verifier filters out stale data points older than the configurable `MAX_DATA_AGE` (default 7 days) during trigger verification and data queries to prevent stale readings from deciding policy payouts.
- Policy Engine holds USDC in escrow: no admin withdrawal function
- Claims Processor is the only address authorized to call `pay_claim` / `expire_policy`
- All monetary arithmetic uses checked arithmetic (Soroban default with overflow-checks = true)

## Event Schema

All state changes publish events using `env.events().publish(topics, data)` to facilitate off-chain indexing and audit trails.

### 1. Policy Engine Events

Topics format: `("policy_created",)`, `("policy_claimed",)`, etc.

| Topic | Event Data Struct | Description |
|---|---|---|
| `initialized` | `Initialized { admin: Address, usdc_token: Address, oracle_address: Address }` | Fired when contract is initialized. |
| `claims_processor_updated` | `ClaimsProcessorUpdated { claims_processor: Address }` | Fired when Claims Processor is registered. |
| `product_created` | `ProductCreated { product_id: u128, name: Symbol, category: Symbol, premium_rate_bps: u32 }` | Fired when a new insurance product is defined. |
| `product_paused` | `ProductPaused { product_id: u128 }` | Fired when a product is paused. |
| `product_deprecated` | `ProductDeprecated { product_id: u128 }` | Fired when a product is deprecated. |
| `policy_created` | `PolicyCreated { policy_id: u128, product_id: u128, policyholder: Address, coverage_amount: i128, premium_paid: i128 }` | Fired when a policy is purchased. |
| `policy_cancelled` | `PolicyCancelled { policy_id: u128, policyholder: Address, refund_amount: i128 }` | Fired when a policy is cancelled and premium refunded. |
| `policy_claimed` | `PolicyClaimed { policy_id: u128, policyholder: Address, coverage_amount: i128 }` | Fired when a claim is paid out to a policyholder. |
| `policy_expired` | `PolicyExpired { policy_id: u128 }` | Fired when a policy expires without trigger payout. |

### 2. Claims Processor Events

Topics format: `("claim_submitted",)`, `("claim_processed",)`, etc.

| Topic | Event Data Struct | Description |
|---|---|---|
| `initialized` | `Initialized { admin: Address, policy_engine: Address, oracle_verifier: Address, staleness_threshold: u64 }` | Fired when Claims Processor is initialized. |
| `claim_submitted` | `ClaimSubmitted { claim_id: u128, policy_id: u128, claimant: Address, coverage_amount: i128 }` | Fired when a claim is manually submitted. |
| `claim_processed` | `ClaimProcessed { claim_id: u128, policy_id: u128, trigger_met: bool, status: ClaimStatus }` | Fired when a claim is settled (Paid or Rejected). |
| `claim_disputed` | `ClaimDisputed { claim_id: u128, claimant: Address, reason: Symbol }` | Fired when a processed claim is disputed. |

### 3. Oracle Verifier Events

Topics format: `("oracle_added",)`, `("oracle_data_submitted",)`, etc.

| Topic | Event Data Struct | Description |
|---|---|---|
| `initialized` | `Initialized { admin: Address }` | Fired when Oracle Verifier is initialized. |
| `oracle_added` | `OracleAdded { oracle: Address, data_type: Symbol, weight: u32 }` | Fired when a new oracle node is registered. |
| `oracle_removed` | `OracleRemoved { oracle: Address, data_type: Symbol }` | Fired when an oracle node is deactivated. |
| `min_confidence_updated` | `MinConfidenceUpdated { threshold: u32 }` | Fired when global minimum confidence changes. |
| `oracle_data_submitted` | `OracleDataSubmitted { oracle: Address, data_type: Symbol, key: Symbol, value: i128, confidence: u32, timestamp: u64 }` | Fired when an oracle submits observation data. |

### 4. Risk Pool Events

Topics format: `("liquidity_deposited",)`, `("premium_distributed",)`, etc.

| Topic | Event Data Struct | Description |
|---|---|---|
| `initialized` | `Initialized { admin: Address, usdc_token: Address, treasury: Address, category: Symbol }` | Fired when Risk Pool is initialized. |
| `liquidity_deposited` | `LiquidityDeposited { provider: Address, amount: i128, shares_minted: i128 }` | Fired when an LP deposits USDC. |
| `liquidity_withdrawn` | `LiquidityWithdrawn { provider: Address, shares_burned: i128, amount_returned: i128 }` | Fired when an LP withdraws USDC. |
| `premium_distributed` | `PremiumDistributed { amount: i128, lp_share: i128, treasury_share: i128 }` | Fired when premium is received and split. |
| `yield_claimed` | `YieldClaimed { provider: Address, amount: i128 }` | Fired when an LP claims yield. |
| `capital_locked` | `CapitalLocked { policy_id: u128, amount: i128 }` | Fired when capital is locked for an active policy. |
| `capital_released` | `CapitalReleased { policy_id: u128, amount: i128 }` | Fired when capital is released back to the pool. |
| `pool_paused` | `PoolPaused { admin: Address }` | Fired when the pool is paused. |
| `pool_resumed` | `PoolResumed { admin: Address }` | Fired when the pool is resumed. |

### 5. Governance DAO Events

Topics format: `("proposal_created",)`, `("vote_cast",)`, etc.

| Topic | Event Data Struct | Description |
|---|---|---|
| `initialized` | `Initialized { admin: Address, gov_token: Address }` | Fired when Governance DAO is initialized. |
| `proposal_created` | `ProposalCreated { proposal_id: u64, proposer: Address, target: Address, function: Symbol }` | Fired when a proposal is proposed. |
| `vote_cast` | `VoteCast { proposal_id: u64, voter: Address, choice: VoteChoice, weight: i128 }` | Fired when a vote is cast. |
| `proposal_finalized` | `ProposalFinalized { proposal_id: u64, status: ProposalStatus }` | Fired when a voting period closes and proposal status changes. |
| `proposal_executed` | `ProposalExecuted { proposal_id: u64 }` | Fired when a proposal is executed. |
| `proposal_cancelled` | `ProposalCancelled { proposal_id: u64 }` | Fired when admin cancels an active proposal. |
| `dao_config_updated` | `DaoConfigUpdated { gov_token: Address, proposal_threshold: i128, total_supply: i128, voting_period: u64 }` | Fired when DAO parameters change. |

