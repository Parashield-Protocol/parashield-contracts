#!/usr/bin/env bash
# register_oracle.sh — Register a new oracle node with the OracleVerifier contract.
#
# Usage:
#   ORACLE_VERIFIER_ID=<contract_id> \
#   ORACLE_ADDRESS=<G...> \
#   DATA_TYPE=weather \
#   WEIGHT=90 \
#   NETWORK=testnet \
#     ./scripts/register_oracle.sh
#
# DATA_TYPE options: weather | flight | onchain | disaster
# WEIGHT range:      1–100 (higher = more influence in median calculation)

set -euo pipefail

ORACLE_VERIFIER_ID="${ORACLE_VERIFIER_ID:-}"
ORACLE_ADDRESS="${ORACLE_ADDRESS:-}"
DATA_TYPE="${DATA_TYPE:-weather}"
WEIGHT="${WEIGHT:-80}"
NETWORK="${NETWORK:-testnet}"
SOURCE="${SOURCE:-deployer}"

[[ -n "$ORACLE_VERIFIER_ID" ]] || { echo "Error: ORACLE_VERIFIER_ID not set" >&2; exit 1; }
[[ -n "$ORACLE_ADDRESS"     ]] || { echo "Error: ORACLE_ADDRESS not set" >&2; exit 1; }

log() { echo "[register-oracle] $*"; }

ADMIN_ADDR=$(stellar keys address "$SOURCE")

log "Registering oracle..."
log "  Contract: $ORACLE_VERIFIER_ID"
log "  Oracle:   $ORACLE_ADDRESS"
log "  Type:     $DATA_TYPE"
log "  Weight:   $WEIGHT / 100"
log "  Network:  $NETWORK"

stellar contract invoke \
  --id "$ORACLE_VERIFIER_ID" \
  --source "$SOURCE" \
  --network "$NETWORK" \
  -- add_oracle \
  --admin "$ADMIN_ADDR" \
  --oracle "$ORACLE_ADDRESS" \
  --data_type "$DATA_TYPE" \
  --weight "$WEIGHT"

log ""
log "Oracle registered successfully."
log "Verify with:"
log "  stellar contract invoke --id $ORACLE_VERIFIER_ID --network $NETWORK -- get_oracles"
log ""
log "Test submission (from the oracle key):"
log "  stellar contract invoke --id $ORACLE_VERIFIER_ID --source <oracle-key> --network $NETWORK -- submit_data \\"
log "    --oracle $ORACLE_ADDRESS --data_type $DATA_TYPE --key 'rainfall:kisumu:2026-06' \\"
log "    --value 450000000 --confidence 92 --timestamp \$(date +%s)"
