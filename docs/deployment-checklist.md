# Deployment Checklist

Complete every step in order. Do not skip.

## Pre-Deployment

- [ ] `make test` passes on the target commit (zero failures)
- [ ] `make lint` passes with zero warnings
- [ ] Soroban SDK version pinned in `Cargo.toml` (no `*` ranges)
- [ ] All contract IDs from Testnet deployment recorded in `.deploy-state`
- [ ] Admin key is a multi-sig or hardware wallet — not a hot key
- [ ] USDC token address verified on the target network
- [ ] At least 2 oracle nodes registered and tested on Testnet

## Testnet Validation

- [ ] `./scripts/deploy_testnet.sh` completes without errors
- [ ] `./scripts/create_products.sh` — all 4 products created
- [ ] `./scripts/register_oracle.sh` — at least 1 oracle per data type
- [ ] Manual policy purchase via CLI succeeds
- [ ] Manual oracle data submission succeeds
- [ ] Manual claim submission succeeds
- [ ] Parametric auto_process triggers a payout
- [ ] LP deposit, yield claim, and withdrawal tested
- [ ] DAO proposal creation, voting, and execution tested

## Mainnet Deployment

- [ ] Read through `./scripts/deploy_mainnet.sh` — understand every step
- [ ] Announce maintenance window in Discord/Telegram
- [ ] Run `./scripts/deploy_mainnet.sh` — type `DEPLOY MAINNET` when prompted
- [ ] Verify each contract ID on Stellar Expert (mainnet)
- [ ] Call `initialize()` on each contract via signed transaction, **in the
      order and with the arguments listed in
      [Contract Initialization Order](#contract-initialization-order)**
- [ ] Wire `set_claims_processor` in PolicyEngine
- [ ] Run `./scripts/check_balances.sh` to verify zero balances before launch
- [ ] Deposit initial USDC liquidity into RiskPool (backstop fund)

## Post-Deployment

- [ ] Update frontend `src/lib/constants.ts` with Mainnet contract IDs
- [ ] Update `ARCHITECTURE.md` with deployed contract addresses
- [ ] Post announcement with contract IDs for community verification
- [ ] Monitor first 24h of claims processing
- [ ] Set up oracle data submission cron jobs

## Contract Initialization Order

All five contracts must be **deployed first** (so every contract ID exists),
then **initialized in the order below**. Contracts store the addresses of
their dependencies at `initialize` time, and every `initialize` can only be
called once (`AlreadyInitialized`). A wrong or missing argument therefore
cannot be corrected by calling `initialize` again; the contract has to be
redeployed. Check every argument before you submit.

Placeholders used below:

| Placeholder | Meaning |
|---|---|
| `$ADMIN` | Admin account (multi-sig or hardware wallet on mainnet). Must sign each `initialize`. |
| `$USDC` | USDC Stellar Asset Contract ID (`C...`) for the target network |
| `$ORACLE_ID`, `$POLICY_ID`, `$RISK_POOL_ID`, `$CLAIMS_ID`, `$DAO_ID` | IDs returned by `stellar contract deploy` (also in `scripts/.deploy-state`) |

### 1. oracle-verifier

No dependencies.

```bash
stellar contract invoke --id "$ORACLE_ID" --source "$SOURCE" --network "$NETWORK" \
  -- initialize \
  --admin "$ADMIN"
```

| Argument | Type | Rules |
|---|---|---|
| `admin` | `Address` | Valid `G...`/`C...` address; must authorize the call |

### 2. policy-engine

Depends on: oracle-verifier (step 1) and the USDC token.

```bash
stellar contract invoke --id "$POLICY_ID" --source "$SOURCE" --network "$NETWORK" \
  -- initialize \
  --admin "$ADMIN" \
  --usdc_token "$USDC" \
  --oracle_address "$ORACLE_ID"
```

| Argument | Type | Rules |
|---|---|---|
| `admin` | `Address` | Valid `G...`/`C...` address |
| `usdc_token` | `Address` | Must be a contract (`C...`). `initialize` calls `balance` and a 1-stroop self-`transfer` on it and fails with `InvalidToken` if either call fails |
| `oracle_address` | `Address` | Oracle-verifier contract ID (`C...`); the zero address is rejected |

### 3. risk-pool

Depends on: policy-engine (step 2), and the claims-processor **contract ID**
(it only needs to be deployed, not yet initialized).

```bash
stellar contract invoke --id "$RISK_POOL_ID" --source "$SOURCE" --network "$NETWORK" \
  -- initialize \
  --admin "$ADMIN" \
  --usdc_token "$USDC" \
  --treasury "$TREASURY" \
  --backstop "$BACKSTOP" \
  --category "$CATEGORY" \
  --policy_engine "$POLICY_ID" \
  --claims_processor "$CLAIMS_ID"
```

| Argument | Type | Rules |
|---|---|---|
| `admin` | `Address` | Valid `G...`/`C...` address, not the zero address; must authorize the call |
| `usdc_token` | `Address` | Same USDC contract passed to policy-engine; `balance` must be callable |
| `treasury` | `Address` | Must be a contract address (`C...`) |
| `backstop` | `Address` | Backstop fund address |
| `category` | `Symbol` | Risk category this pool covers (e.g. `weather`); must not be empty |
| `policy_engine` | `Address` | Policy-engine contract ID from step 2 |
| `claims_processor` | `Address` | Claims-processor contract ID (initialized in step 4) |

### 4. claims-processor

Depends on: oracle-verifier, policy-engine and risk-pool (steps 1–3).

```bash
stellar contract invoke --id "$CLAIMS_ID" --source "$SOURCE" --network "$NETWORK" \
  -- initialize \
  --admin "$ADMIN" \
  --policy_engine "$POLICY_ID" \
  --risk_pool "$RISK_POOL_ID" \
  --oracle_verifier "$ORACLE_ID" \
  --staleness_threshold 604800
```

| Argument | Type | Rules |
|---|---|---|
| `admin` | `Address` | Valid Stellar address; must authorize the call |
| `policy_engine` | `Address` | Policy-engine contract ID from step 2 |
| `risk_pool` | `Address` | Risk-pool contract ID from step 3 |
| `oracle_verifier` | `Address` | Oracle-verifier contract ID from step 1 |
| `staleness_threshold` | `u64` | Maximum age of oracle data, in seconds, accepted when settling a claim (`604800` = 7 days) |

### 5. Post-initialization wiring

Run these immediately after step 4, before any product is created:

- [ ] `policy-engine.set_claims_processor(admin, $CLAIMS_ID)`
- [ ] `claims-processor.add_keeper(admin, $KEEPER)` for each keeper. Without
      at least one keeper, no claim can be settled.
- [ ] `oracle-verifier.add_oracle(admin, oracle, data_type, weight)` for each
      oracle (`./scripts/register_oracle.sh`)

### 6. governance-dao (independent)

Has no initialization dependency on the four contracts above, but
initialize it before handing admin rights to governance.

```bash
stellar contract invoke --id "$DAO_ID" --source "$SOURCE" --network "$NETWORK" \
  -- initialize \
  --admin "$ADMIN" \
  --config '{"gov_token": "<C...>", "proposal_threshold": "<i128>", "total_supply": "<i128>", "quorum_bps": <u32>, "majority_bps": <u32>, "voting_period": <u64>, "proposal_timelock": <u64>, "discussion_period": <u64>, "vote_weight_cap": "<i128>"}'
```

| `config` field | Rules |
|---|---|
| `gov_token` | Token contract (`C...`); its balances are voting weight |
| `quorum_bps` | `1000`–`10000` (10%–100%); lower values are rejected with `QuorumTooLow` |
| `voting_period` | `3600` (1 hour) to `2592000` (30 days) seconds |
| `discussion_period` | Seconds; `0` disables the discussion phase |
| `vote_weight_cap` | `0` = no cap |

Optionally call `governance-dao.set_risk_pool(admin, $RISK_POOL_ID)` so LP
share delegation counts toward voting weight.

### Verification

- [ ] Each contract's `initialize` transaction succeeded. Re-running it must
      fail with `AlreadyInitialized`.
- [ ] `get_version()` returns `1` on every contract.
- [ ] Spot-check stored dependencies against `scripts/.deploy-state` before
      opening the protocol to users.
