# parashield-contracts

Soroban smart contracts for the Parashield parametric insurance protocol on Stellar.

Parametric insurance replaces claims adjusters with oracle data. If the measured condition is met — rainfall below threshold, flight delayed, exploit detected — the contract transfers USDC to the policyholder automatically. No filing, no review, no waiting.

---

### Dependency order

```
oracle-verifier
      ↑
policy-engine ← (oracle-verifier address passed at init)
      ↑
claims-processor ← (policy-engine + oracle-verifier addresses passed at init)
```

---

## How a policy works

```
1.  Admin creates product — e.g. "Kisumu rainfall < 50mm, 5% premium, max 1000 USDC"
2.  Buyer calls buy_policy(product_id, coverage=100 USDC, duration=30 days, key="kis2606")
      → 5 USDC premium pulled from buyer into policy-engine contract
3.  Oracle backend runs hourly:
      oracle-verifier.submit_data("weather", "kis2606", 32_000_000, 95, ts)
      (32mm observed — 7-decimal fixed point: 32_000_000 = 32.0000000)
4.  Keeper calls claims-processor.auto_process(policy_id)
      → verify_trigger("weather", "kis2606", {threshold: 50_000_000, comparison: LessThan})
      → 32mm < 50mm → true
      → policy-engine.pay_claim(policy_id)
      → 100 USDC transferred to policyholder
```

---


## Contracts

```
contracts/
├── oracle-verifier     authorized oracles submit readings; median aggregation; verify_trigger
├── policy-engine       products, policies, USDC escrow, payout execution
├── claims-processor    reads oracle data, evaluates trigger, calls pay_claim / expire_policy
├── risk-pool           LP capital provisioning — v2, not yet implemented
└── governance-dao      on-chain parameter governance — v2, not yet implemented
```


## A note on Claimable Balances

The original pitch described encoding trigger conditions inside Stellar's `ClaimPredicate`. This is not possible — `ClaimPredicate` only supports time-based conditions (`before`, `after`). There is no predicate type for external data.

The policy-engine contract holds USDC directly and executes `token.transfer` to the policyholder when the claims-processor confirms a trigger. This is functionally identical to the Claimable Balance concept described in the pitch, and is the correct implementation.

---

## Contract API

### oracle-verifier

| Function | Auth | Description |
|---|---|---|
| `initialize(admin)` | — | one-time setup |
| `add_oracle(admin, oracle, data_type, weight)` | admin | register oracle wallet |
| `remove_oracle(admin, oracle, data_type)` | admin | soft-deactivate |
| `submit_data(oracle, data_type, key, value, confidence, ts)` | oracle | submit reading |
| `verify_trigger(data_type, key, condition) → bool` | public | used by claims-processor |
| `get_data(data_type, key) → OracleDataPoint` | public | latest reading |
| `get_aggregated(data_type, key) → AggregatedData` | public | median across all oracles |

`value` is 7-decimal fixed point. `50mm rainfall = 500_000_000`.

`TriggerCondition { data_type, key, threshold, comparison: LessThan | GreaterThan | Equal }`

### policy-engine

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, usdc_token, oracle)` | — | one-time setup |
| `set_claims_processor(admin, cp)` | admin | wire the claims-processor |
| `create_product(admin, params)` | admin | define new insurance product |
| `pause_product(admin, id)` | admin | block new purchases |
| `buy_policy(buyer, product_id, coverage, duration_days, oracle_key) → u128` | buyer | purchase, pulls premium |
| `cancel_policy(policyholder, id) → i128` | policyholder | refund premium |
| `pay_claim(claims_processor, id)` | claims-processor | transfer coverage to policyholder |
| `expire_policy(claims_processor, id)` | claims-processor | mark expired, no payout |
| `get_policy(id) → Policy` | public | read policy record |
| `get_product(id) → InsuranceProduct` | public | read product definition |
| `get_user_policies(user) → Vec<u128>` | public | policy IDs for a wallet |

Premium: `coverage_amount × premium_rate_bps / 10_000`

### claims-processor

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, policy_engine, oracle_verifier)` | — | one-time setup |
| `auto_process(keeper, policy_id) → ClaimResult` | keeper | primary path — no user action needed |
| `submit_claim(claimant, policy_id) → u128` | policyholder | manual trigger |
| `process_claim(keeper, claim_id) → ClaimResult` | keeper | evaluate after submit_claim |
| `dispute_claim(claimant, claim_id, reason)` | claimant | flag for review |

`ClaimResult: Paid | Rejected | Expired | AlreadyClaimed | PolicyNotActive`

`auto_process` is idempotent — calling it twice on an already-settled policy returns `AlreadyClaimed` without touching state.

---

## Setup

```bash
# Install wasm target
rustup target add wasm32v1-none

# Install Stellar CLI
cargo install --locked stellar-cli --features opt
```

```bash
# Build
cd contracts && cargo build --target wasm32v1-none --release

# Test
cargo test
```

Test results: 14 + 12 + 7 = **33 tests, all passing**.

```bash
# Deploy to testnet
stellar keys generate deployer --network testnet
stellar keys fund deployer --network testnet
./scripts/deploy_testnet.sh
```

---

## Storage layout

All contracts use two Soroban storage tiers:

- `instance` — contract config: admin address, linked contract addresses, ID counters
- `persistent` — data that must survive contract upgrades: policy records, oracle submissions, claim records

Neither tier is time-bounded in v1. TTL extension is a v2 consideration.

---

## Security

- Every admin function calls `require_auth()` and checks the caller against the stored admin address.
- `pay_claim` and `expire_policy` on the policy-engine are restricted to the registered claims-processor address. No external party can drain funds.
- `auto_process` is idempotent — double-calling cannot produce a double-payout.
- Oracle submissions are restricted to registered oracle wallets. Unregistered callers panic with `OracleNotRegistered`.

---

## Related

- [parashield-backend](https://github.com/Parashield-Protocol/parashield-backend) — keeper daemon, oracle ingestion, REST API
- [parashield-frontend](https://github.com/Parashield-Protocol/parashield-frontend) — Next.js marketplace UI
