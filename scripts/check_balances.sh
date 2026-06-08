#!/usr/bin/env bash
# check_balances.sh — Quick health check on deployed contract balances.
#
# Usage:
#   POLICY_ENGINE_ID=<id> RISK_POOL_ID=<id> USDC_ID=<id> NETWORK=testnet \
#     ./scripts/check_balances.sh

set -euo pipefail

POLICY_ENGINE_ID="${POLICY_ENGINE_ID:-}"
RISK_POOL_ID="${RISK_POOL_ID:-}"
USDC_ID="${USDC_ID:-}"
NETWORK="${NETWORK:-testnet}"

[[ -n "$USDC_ID" ]] || { echo "Error: USDC_ID not set" >&2; exit 1; }

log() { echo "[check-balances] $*"; }

STROOPS=10000000

format_usdc() {
  local raw="$1"
  echo "scale=7; $raw / $STROOPS" | bc
}

log "=== Parashield Balance Check (${NETWORK}) ==="
echo ""

if [[ -n "$POLICY_ENGINE_ID" ]]; then
  RAW=$(stellar contract invoke \
    --id "$USDC_ID" \
    --network "$NETWORK" \
    -- balance \
    --id "$POLICY_ENGINE_ID" 2>/dev/null || echo "0")
  log "PolicyEngine USDC balance: $(format_usdc "$RAW") USDC (${RAW} stroops)"
fi

if [[ -n "$RISK_POOL_ID" ]]; then
  RAW=$(stellar contract invoke \
    --id "$USDC_ID" \
    --network "$NETWORK" \
    -- balance \
    --id "$RISK_POOL_ID" 2>/dev/null || echo "0")
  log "RiskPool USDC balance:     $(format_usdc "$RAW") USDC (${RAW} stroops)"

  STATS=$(stellar contract invoke \
    --id "$RISK_POOL_ID" \
    --network "$NETWORK" \
    -- get_stats 2>/dev/null || echo "{}")
  log "RiskPool stats:            $STATS"
fi

log ""
log "Done."
