# Parashield — Contracts

**Decentralized parametric insurance on Stellar. Automatic payouts triggered by real-world data — no claims adjuster, no delays, no denials.**

## What is parametric insurance?

Traditional insurance: event occurs → file claim → adjuster investigates → payment (maybe, 30-90 days).

Parashield: event occurs → oracle confirms data → smart contract auto-pays within seconds.

## Contracts

| Contract | Status | Purpose |
|---|---|---|
| `oracle-verifier` | ✅ Implemented | Stores oracle data submissions; aggregates; exposes `verify_trigger` |
| `policy-engine` | ✅ Implemented | Manages products and policies; holds USDC escrow; executes payouts |
| `claims-processor` | ✅ Implemented | Evaluates triggers against oracle data; instructs Policy Engine to pay |
| `risk-pool` | 🔧 v2 stub | LP capital provisioning, yield distribution |
| `governance-dao` | 🔧 v2 stub | Token-weighted protocol governance |

## Architecture Note — Claimable Balances

The pitch document describes using Stellar's Claimable Balance predicates to encode trigger conditions (e.g., `rainfall < 50mm`). **This is architecturally incorrect.** Stellar's `ClaimPredicate` only supports time-based conditions (`before`, `after`), not data-driven conditions.

In v1 Parashield, the Soroban Policy Engine contract acts as the escrow — it holds USDC and transfers directly when the Claims Processor confirms a trigger. This is simpler and more flexible than Claimable Balances.

See [ARCHITECTURE.md](./ARCHITECTURE.md) for full design rationale.

## Data Flow (Crop Insurance)

```
1. Admin deploys:   oracle-verifier → policy-engine → claims-processor
2. Admin wires:     policy-engine.set_claims_processor(claims_id)
3. Admin creates:   policy-engine.create_product({
                      oracle_data_type: "weather",
                      trigger_threshold: 50_000_000,   // 50mm
                      trigger_comparison: LessThan,
                      premium_rate_bps: 500,            // 5%
                    })
4. Farmer buys:     policy-engine.buy_policy(product_id, coverage=100 USDC, duration=30 days,
                      oracle_key: "kis2606")
                    → 5 USDC premium transferred from farmer to contract

5. Oracle submits:  oracle-verifier.submit_data("weather", "kis2606", 32_000_000, 95, ts)
                    // 32mm < 50mm threshold

6. Keeper processes: claims-processor.auto_process(policy_id)
                    → reads oracle: 32mm < 50mm → trigger MET
                    → policy-engine.pay_claim(policy_id)
                    → 100 USDC transferred to farmer wallet
```

## Setup

### Prerequisites
- Rust + `wasm32v1-none` target
- Stellar CLI 25+

```bash
# Install Rust target
rustup target add wasm32v1-none

# Install Stellar CLI
cargo install --locked stellar-cli --features opt
```

### Build

```bash
cd contracts
cargo build --target wasm32v1-none --release
```

### Test

```bash
cd contracts
cargo test
```

Expected output:
```
test result: ok. 14 passed — oracle-verifier
test result: ok. 12 passed — policy-engine
test result: ok.  7 passed — claims-processor
```

### Deploy to Testnet

```bash
# Fund your identity first
stellar keys generate deployer --network testnet
stellar keys fund deployer --network testnet

# Deploy contracts in order
./scripts/deploy_testnet.sh
```

## Contract APIs

### oracle-verifier
```
initialize(admin)
add_oracle(admin, oracle, data_type, weight)
submit_data(oracle, data_type, key, value, confidence, timestamp)
verify_trigger(data_type, key, condition) → bool
get_data(data_type, key) → OracleDataPoint
get_aggregated(data_type, key) → AggregatedData
```

### policy-engine
```
initialize(admin, usdc_token, oracle_address)
set_claims_processor(admin, claims_processor)
create_product(admin, params: CreateProductParams) → u128
buy_policy(buyer, product_id, coverage_amount, duration_days, oracle_key) → u128
cancel_policy(policyholder, policy_id) → i128
pay_claim(claims_processor, policy_id)     ← called by claims-processor
expire_policy(claims_processor, policy_id) ← called by claims-processor
get_policy(policy_id) → Policy
get_product(product_id) → InsuranceProduct
```

### claims-processor
```
initialize(admin, policy_engine, oracle_verifier)
submit_claim(claimant, policy_id) → u128
process_claim(keeper, claim_id) → ClaimResult
auto_process(keeper, policy_id) → ClaimResult   ← primary path
dispute_claim(claimant, claim_id, reason)
get_claim(claim_id) → Claim
get_pending_claims() → Vec<u128>
```

## Insurance Products Supported (v1)

| Category | Trigger | Oracle data_type | Example key |
|---|---|---|---|
| Crop / Rainfall | `rainfall < threshold` | `"weather"` | `"kis2606"` (Kisumu June 2026) |
| Storm | `wind_speed > threshold` | `"weather"` | `"sto2606"` |
| Flight delay | `delay > threshold` | `"flight"` | `"kq100_260615"` |
| DeFi cover | `exploit_detected == 1` | `"onchain"` | `"soroswap_v1"` |

## Roadmap

- **v1** (this): oracle-verifier, policy-engine, claims-processor with crop insurance
- **v2**: risk-pool with LP share tokens; pool token trading on Stellar DEX
- **v3**: governance-dao with SHIELD token; on-chain parameter voting
- **v4**: SEP-24 fiat on/off ramp; MoneyGram cash-out integration

## License

MIT
