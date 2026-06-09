#!/usr/bin/env bash
# submit_oracle_data.sh — Submit a data point to the OracleVerifier contract.
#
# Usage:
#   ORACLE_VERIFIER_ID=<id> ORACLE_KEY=<stellar-key-name> \
#   DATA_TYPE=weather KEY=kis2606 VALUE=30000000 CONFIDENCE=92 \
#   NETWORK=testnet \
#     ./scripts/submit_oracle_data.sh
#
# VALUE is in 7-decimal fixed point: 30000000 = 30.0000000 mm rainfall

set -euo pipefail

ORACLE_VERIFIER_ID="${ORACLE_VERIFIER_ID:-}"
ORACLE_KEY="${ORACLE_KEY:-oracle}"
DATA_TYPE="${DATA_TYPE:-weather}"
KEY="${KEY:-}"
VALUE="${VALUE:-}"
CONFIDENCE="${CONFIDENCE:-80}"
NETWORK="${NETWORK:-testnet}"

[[ -n "$ORACLE_VERIFIER_ID" ]] || { echo "Error: ORACLE_VERIFIER_ID not set" >&2; exit 1; }
[[ -n "$KEY"   ]] || { echo "Error: KEY not set (e.g. kis2606)" >&2; exit 1; }
[[ -n "$VALUE" ]] || { echo "Error: VALUE not set (7-decimal stroops)" >&2; exit 1; }

ORACLE_ADDR=$(stellar keys address "$ORACLE_KEY")
TIMESTAMP=$(date +%s)

log() { echo "[oracle-submit] $*"; }

log "Submitting oracle data:"
log "  Contract:   $ORACLE_VERIFIER_ID"
log "  Oracle:     $ORACLE_ADDR"
log "  Data type:  $DATA_TYPE"
log "  Key:        $KEY"
log "  Value:      $VALUE (= $(echo "scale=7; $VALUE / 10000000" | bc))"
log "  Confidence: $CONFIDENCE / 100"
log "  Timestamp:  $TIMESTAMP ($(date -r "$TIMESTAMP" 2>/dev/null || date -d "@$TIMESTAMP"))"
log "  Network:    $NETWORK"

stellar contract invoke \
  --id "$ORACLE_VERIFIER_ID" \
  --source "$ORACLE_KEY" \
  --network "$NETWORK" \
  -- submit_data \
  --oracle "$ORACLE_ADDR" \
  --data_type "$DATA_TYPE" \
  --key "$KEY" \
  --value "$VALUE" \
  --confidence "$CONFIDENCE" \
  --timestamp "$TIMESTAMP"

log ""
log "Data submitted successfully."
