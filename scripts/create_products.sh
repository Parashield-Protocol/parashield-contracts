#!/usr/bin/env bash
# create_products.sh — Seed the four Parashield insurance products.
#
# Usage:
#   POLICY_ENGINE_ID=<contract_id> NETWORK=testnet ./scripts/create_products.sh
#
# Prerequisites:
#   - stellar CLI installed and configured
#   - Policy Engine contract initialized
#   - SOURCE key must be the admin key used in initialize()

set -euo pipefail

POLICY_ID="${POLICY_ENGINE_ID:-}"
NETWORK="${NETWORK:-testnet}"
SOURCE="${SOURCE:-deployer}"

[[ -n "$POLICY_ID" ]] || { echo "Error: POLICY_ENGINE_ID not set" >&2; exit 1; }

log() { echo "[create-products] $*"; }

invoke() {
  stellar contract invoke \
    --id "$POLICY_ID" \
    --source "$SOURCE" \
    --network "$NETWORK" \
    -- "$@"
}

ADMIN_ADDR=$(stellar keys address "$SOURCE")

# ── Product 1: Crop Drought Insurance ─────────────────────────────────────────
log "Creating: Crop Drought Insurance"
invoke create_product \
  --admin "$ADMIN_ADDR" \
  --params '{
    "name": "Crop_Drought_Insurance",
    "category": "crop",
    "trigger_type": "Threshold",
    "oracle_data_type": "weather",
    "premium_rate_bps": 300,
    "coverage_min": 1000000000,
    "coverage_max": 100000000000,
    "max_duration_days": 180,
    "trigger_threshold": 500000000,
    "trigger_comparison": "LessThan"
  }'

# ── Product 2: Flight Delay Insurance ────────────────────────────────────────
log "Creating: Flight Delay Insurance"
invoke create_product \
  --admin "$ADMIN_ADDR" \
  --params '{
    "name": "Flight_Delay_Insurance",
    "category": "flight",
    "trigger_type": "Binary",
    "oracle_data_type": "flight",
    "premium_rate_bps": 150,
    "coverage_min": 100000000,
    "coverage_max": 10000000000,
    "max_duration_days": 2,
    "trigger_threshold": 1200000000,
    "trigger_comparison": "GreaterThan"
  }'

# ── Product 3: DeFi Protocol Hack Insurance ───────────────────────────────────
log "Creating: DeFi Protocol Hack Insurance"
invoke create_product \
  --admin "$ADMIN_ADDR" \
  --params '{
    "name": "DeFi_Hack_Insurance",
    "category": "defi",
    "trigger_type": "Parametric",
    "oracle_data_type": "onchain",
    "premium_rate_bps": 500,
    "coverage_min": 10000000000,
    "coverage_max": 1000000000000,
    "max_duration_days": 365,
    "trigger_threshold": 500000000,
    "trigger_comparison": "GreaterThan"
  }'

# ── Product 4: Natural Disaster Insurance ────────────────────────────────────
log "Creating: Natural Disaster Insurance"
invoke create_product \
  --admin "$ADMIN_ADDR" \
  --params '{
    "name": "Natural_Disaster_Insurance",
    "category": "disaster",
    "trigger_type": "Threshold",
    "oracle_data_type": "disaster",
    "premium_rate_bps": 200,
    "coverage_min": 5000000000,
    "coverage_max": 500000000000,
    "max_duration_days": 365,
    "trigger_threshold": 60000000,
    "trigger_comparison": "GreaterThan"
  }'

log ""
log "=== 4 products created successfully ==="
log "Verify with:"
log "  stellar contract invoke --id $POLICY_ID --network $NETWORK -- get_active_products"
