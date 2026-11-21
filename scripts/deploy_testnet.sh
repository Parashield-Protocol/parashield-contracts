#!/usr/bin/env bash
# Deploy Parashield contracts to Stellar testnet in dependency order.
# Usage:
#   stellar keys generate deployer --network testnet
#   stellar keys fund deployer --network testnet
#   ./scripts/deploy_testnet.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NETWORK="${NETWORK:-testnet}"
SOURCE="${SOURCE:-deployer}"
WASM_DIR="$ROOT/contracts/target/wasm32v1-none/release"
STATE_FILE="$ROOT/scripts/.deploy-state"

log()  { echo "[deploy] $*"; }
die()  { echo "[deploy] ERROR: $*" >&2; exit 1; }

state_get() { grep "^$1=" "$STATE_FILE" 2>/dev/null | cut -d= -f2- || true; }
state_set() { grep -v "^$1=" "$STATE_FILE" 2>/dev/null > "$STATE_FILE.tmp" || true; echo "$1=$2" >> "$STATE_FILE.tmp"; mv "$STATE_FILE.tmp" "$STATE_FILE"; }

touch "$STATE_FILE"

# ── Build ─────────────────────────────────────────────────────────────────────

log "Building contracts..."
cd "$ROOT/contracts"
cargo build --target wasm32v1-none --release --quiet
log "Build complete."

# ── Helper: deploy or skip ────────────────────────────────────────────────────

deploy_contract() {
    local name="$1"
    local wasm="$WASM_DIR/${name//-/_}.wasm"

    local existing; existing=$(state_get "$name")
    if [ -n "$existing" ]; then
        log "$name already deployed at $existing — skipping."
        echo "$existing"
        return
    fi

    [ -f "$wasm" ] || die "WASM not found: $wasm"
    log "Deploying $name..."
    local addr
    addr=$(stellar contract deploy \
        --wasm "$wasm" \
        --source "$SOURCE" \
        --network "$NETWORK" \
        2>/dev/null)
    state_set "$name" "$addr"
    log "$name → $addr"
    echo "$addr"
}

# ── Deploy in dependency order ────────────────────────────────────────────────

ORACLE_ID=$(deploy_contract "parashield-oracle-verifier")
RISK_POOL_ID=$(deploy_contract "parashield-risk-pool")
POLICY_ID=$(deploy_contract "parashield-policy-engine")
CLAIMS_ID=$(deploy_contract "parashield-claims-processor")
DAO_ID=$(deploy_contract "parashield-governance-dao")

# ── Get USDC testnet address ──────────────────────────────────────────────────

# Stellar testnet USDC SAC contract address (Circle-issued: USDC:GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5)
USDC_TESTNET="${USDC_TESTNET:-CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA}"

# ── Initialize contracts ──────────────────────────────────────────────────────

DEPLOYER_ADDR=$(stellar keys address "$SOURCE")

log "Initializing oracle-verifier..."
stellar contract invoke \
    --id "$ORACLE_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- initialize \
    --admin "$DEPLOYER_ADDR"

log "Initializing policy-engine..."
stellar contract invoke \
    --id "$POLICY_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- initialize \
    --admin "$DEPLOYER_ADDR" \
    --usdc_token "$USDC_TESTNET" \
    --oracle_address "$ORACLE_ID"

log "Initializing risk-pool..."
stellar contract invoke \
    --id "$RISK_POOL_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- initialize \
    --admin "$DEPLOYER_ADDR"

log "Initializing claims-processor..."
stellar contract invoke \
    --id "$CLAIMS_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- initialize \
    --admin "$DEPLOYER_ADDR" \
    --policy_engine "$POLICY_ID" \
    --risk_pool "$RISK_POOL_ID" \
    --oracle_verifier "$ORACLE_ID" \
    --staleness_threshold 604800

log "Wiring claims-processor into policy-engine..."
stellar contract invoke \
    --id "$POLICY_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- set_claims_processor \
    --admin "$DEPLOYER_ADDR" \
    --claims_processor "$CLAIMS_ID"

log "Initializing governance-dao..."
# Testnet governance configuration:
# - proposal_threshold: 100 tokens (lower than mainnet for testing)
# - total_supply: 10M tokens (smaller supply for testnet)
# - quorum_bps: 500 = 5% quorum (lower for easier testnet testing)
# - majority_bps: 5100 = 51% majority
# - voting_period: 86400 = 1 day (shorter for faster testing)
# - proposal_timelock: 3600 = 1 hour (shorter for faster testing)
# - discussion_period: 1800 = 30 minutes
# - vote_weight_cap: 0 = no cap
stellar contract invoke \
    --id "$DAO_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- initialize \
    --admin "$DEPLOYER_ADDR" \
    --config '{"gov_token": "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC", "proposal_threshold": "1000000000", "total_supply": "10000000000000000", "quorum_bps": 500, "majority_bps": 5100, "voting_period": 86400, "proposal_timelock": 3600, "discussion_period": 1800, "vote_weight_cap": 0}'

# ── Summary ───────────────────────────────────────────────────────────────────

log ""
log "=== Deployment complete ==="
log "Network:          $NETWORK"
log "Oracle Verifier:  $ORACLE_ID"
log "Risk Pool:        $RISK_POOL_ID"
log "Policy Engine:    $POLICY_ID"
log "Claims Processor: $CLAIMS_ID"
log "Governance DAO:   $DAO_ID"
log ""
log "Next steps:"
log "  1. Register an oracle: stellar contract invoke --id $ORACLE_ID ... -- add_oracle --oracle <ADDR> --data_type weather --weight 90"
log "  2. Create a product:   stellar contract invoke --id $POLICY_ID ... -- create_product ..."
log "  3. Fund policy-engine with USDC coverage capital (until Risk Pool v2)"
log ""
log "State saved to $STATE_FILE"
