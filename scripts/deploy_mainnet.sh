#!/usr/bin/env bash
# deploy_mainnet.sh — Production deployment with safety checks.
#
# DANGER: This script targets Stellar Mainnet (Pubnet).
# Ensure you have thoroughly tested on Testnet before running.
#
# Prerequisites:
#   - stellar CLI installed
#   - DEPLOYER_SECRET set to a key NAME (not the secret key itself)
#     Generate one with: stellar keys generate deployer --network mainnet
#   - All contracts passing `make test` and `make lint`

set -euo pipefail

NETWORK="mainnet"
RPC="https://soroban-mainnet.stellar.org"
PASSPHRASE="Public Global Stellar Network ; September 2015"
SOURCE="${DEPLOYER_SECRET:-}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_DIR="$ROOT/contracts/target/wasm32v1-none/release"
STATE_FILE="$ROOT/scripts/.deploy-mainnet-state"

log()  { echo "[mainnet-deploy] $*"; }
die()  { echo "[mainnet-deploy] FATAL: $*" >&2; exit 1; }

# ── Safety gate ───────────────────────────────────────────────────────────────

if [[ -z "$SOURCE" ]]; then
  die "DEPLOYER_SECRET is not set. Set it to a key NAME (e.g., 'deployer'), not the secret key itself."
fi

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║      PARASHIELD MAINNET DEPLOYMENT — THIS IS IRREVERSIBLE    ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""
echo "Network:  $NETWORK"
echo "RPC:      $RPC"
echo ""
read -p "Have you tested all contracts on Testnet? (yes/no): " CONFIRM
[[ "$CONFIRM" == "yes" ]] || die "Aborted."

read -p "Type 'DEPLOY MAINNET' to proceed: " FINAL
[[ "$FINAL" == "DEPLOY MAINNET" ]] || die "Aborted."

# ── Build ─────────────────────────────────────────────────────────────────────

log "Running tests before deploy..."
cd "$ROOT/contracts"
if ! cargo test --quiet 2>&1 | tail -5; then
  die "Tests failed"
fi
log "Tests passed."

log "Building optimized WASM..."
cargo build --target wasm32v1-none --release --quiet
log "Build complete."

touch "$STATE_FILE"

state_get() { grep "^$1=" "$STATE_FILE" 2>/dev/null | cut -d= -f2- || true; }
state_set() {
  grep -v "^$1=" "$STATE_FILE" 2>/dev/null > "$STATE_FILE.tmp" || true
  echo "$1=$2" >> "$STATE_FILE.tmp"
  mv "$STATE_FILE.tmp" "$STATE_FILE"
}

deploy_contract() {
  local name="$1"
  local wasm="$WASM_DIR/${name//-/_}.wasm"
  local existing; existing=$(state_get "$name")
  if [[ -n "$existing" ]]; then
    log "$name already deployed at $existing — skipping."
    echo "$existing"; return
  fi
  [[ -f "$wasm" ]] || die "WASM not found: $wasm"
  log "Deploying $name to Mainnet..."
  local addr
  addr=$(stellar contract deploy \
    --wasm "$wasm" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    --rpc-url "$RPC" \
    --network-passphrase "$PASSPHRASE" \
    2>/dev/null)
  state_set "$name" "$addr"
  log "$name → $addr"
  echo "$addr"
}

# ── Deploy ────────────────────────────────────────────────────────────────────

ORACLE_ID=$(deploy_contract "parashield-oracle-verifier")
POLICY_ID=$(deploy_contract "parashield-policy-engine")
CLAIMS_ID=$(deploy_contract "parashield-claims-processor")
POOL_ID=$(deploy_contract "parashield-risk-pool")
DAO_ID=$(deploy_contract "parashield-governance-dao")

# ── Initialize Governance DAO ─────────────────────────────────────────────────

log "Initializing Governance DAO..."

# TODO: Replace these placeholder values with actual governance configuration
# GOV_TOKEN_ADDRESS: deployed governance token contract address
# ADMIN_ADDRESS: DAO administrator address (can be same as deployer or multisig)
# These should be set via environment variables before running this script

if [[ -z "${GOV_TOKEN_ADDRESS:-}" ]]; then
  log "WARNING: GOV_TOKEN_ADDRESS not set. Skipping DAO initialization."
  log "The DAO contract is deployed but not initialized."
  log "Run the following command manually to initialize:"
  log "  stellar contract invoke --id $DAO_ID \\"
  log "    --source \$DEPLOYER_SECRET \\"
  log "    --network mainnet \\"
  log "    -- initialize \\"
  log "      --admin <ADMIN_ADDRESS> \\"
  log "      --config '{\"gov_token\": \"<GOV_TOKEN_ADDRESS>\", \"proposal_threshold\": \"1000000000\", \"total_supply\": \"100000000000000\", \"quorum_bps\": 1000, \"majority_bps\": 5100, \"voting_period\": 604800, \"proposal_timelock\": 172800, \"discussion_period\": 86400, \"vote_weight_cap\": 0}'"
else
  # Default DAO configuration values (adjust as needed):
  # - proposal_threshold: 1,000 tokens (1000000000 stroops with 7 decimals)
  # - total_supply: 100M tokens (100000000000000 stroops)
  # - quorum_bps: 1000 = 10% of total supply must vote
  # - majority_bps: 5100 = 51% of votes must be FOR
  # - voting_period: 604800 seconds = 7 days
  # - proposal_timelock: 172800 seconds = 2 days
  # - discussion_period: 86400 seconds = 1 day
  # - vote_weight_cap: 0 = no cap (adjust to prevent whale dominance)
  
  ADMIN_ADDR="${DAO_ADMIN_ADDRESS:-$SOURCE}"
  
  stellar contract invoke \
    --id "$DAO_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    --rpc-url "$RPC" \
    --network-passphrase "$PASSPHRASE" \
    -- initialize \
      --admin "$ADMIN_ADDR" \
      --config "{\"gov_token\": \"$GOV_TOKEN_ADDRESS\", \"proposal_threshold\": \"1000000000\", \"total_supply\": \"100000000000000\", \"quorum_bps\": 1000, \"majority_bps\": 5100, \"voting_period\": 604800, \"proposal_timelock\": 172800, \"discussion_period\": 86400, \"vote_weight_cap\": 0}" \
    2>&1 | tail -5
  
  log "Governance DAO initialized."
fi

log ""
log "=== Mainnet deployment complete ==="
log "Oracle Verifier:  $ORACLE_ID"
log "Policy Engine:    $POLICY_ID"
log "Claims Processor: $CLAIMS_ID"
log "Risk Pool:        $POOL_ID"
log "Governance DAO:   $DAO_ID"
log ""
log "State saved to: $STATE_FILE"
log "IMPORTANT: Back up $STATE_FILE — it contains the only record of contract IDs."
