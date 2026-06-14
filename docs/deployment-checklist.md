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
- [ ] Call `initialize()` on each contract via signed transaction
- [ ] Wire `set_claims_processor` in PolicyEngine
- [ ] Run `./scripts/check_balances.sh` to verify zero balances before launch
- [ ] Deposit initial USDC liquidity into RiskPool (backstop fund)

## Post-Deployment

- [ ] Update frontend `src/lib/constants.ts` with Mainnet contract IDs
- [ ] Update `ARCHITECTURE.md` with deployed contract addresses
- [ ] Post announcement with contract IDs for community verification
- [ ] Monitor first 24h of claims processing
- [ ] Set up oracle data submission cron jobs
