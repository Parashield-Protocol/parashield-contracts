#!/usr/bin/env bash
# validate_deploy_args.sh — CI test to verify deployment script initialization
# arguments match contract function signatures.
#
# This catches signature changes that would break deploy scripts silently.
# Run after building WASM contracts.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_DIR="$ROOT/contracts/target/wasm32v1-none/release"
FAILED=0

log() { echo "[validate-deploy] $*"; }
err() { echo "[validate-deploy] ERROR: $*" >&2; FAILED=1; }

# Check if stellar CLI is available
if ! command -v stellar &> /dev/null; then
    err "stellar CLI not found. Install from https://github.com/stellar/stellar-cli"
    exit 1
fi

# Ensure WASM files exist
if [ ! -d "$WASM_DIR" ] || [ -z "$(ls -A "$WASM_DIR"/*.wasm 2>/dev/null)" ]; then
    err "WASM files not found in $WASM_DIR. Run 'cargo build --target wasm32v1-none --release' first."
    exit 1
fi

log "Validating deployment script arguments against contract signatures..."

# Helper: extract function signature from WASM using stellar contract inspect
get_function_signature() {
    local wasm="$1"
    local function_name="$2"
    
    if [ ! -f "$wasm" ]; then
        echo ""
        return
    fi
    
    # Use stellar contract inspect to get the function signature
    stellar contract inspect --wasm "$wasm" 2>/dev/null | \
        awk -v fn="$function_name" '
            $0 ~ "fn " fn "\\(" { 
                print; 
                while (getline) { 
                    print; 
                    if ($0 ~ /\);$/) break; 
                } 
            }
        '
}

# Helper: validate that expected args appear in function signature
validate_args() {
    local contract="$1"
    local function="$2"
    shift 2
    local expected_args=("$@")
    
    local wasm="$WASM_DIR/${contract//-/_}.wasm"
    log "Checking $contract::$function"
    
    local signature
    signature=$(get_function_signature "$wasm" "$function")
    
    if [ -z "$signature" ]; then
        err "$contract::$function not found in WASM"
        return
    fi
    
    log "  Signature: $(echo "$signature" | tr '\n' ' ' | sed 's/  */ /g')"
    
    for arg in "${expected_args[@]}"; do
        if ! echo "$signature" | grep -q "$arg"; then
            err "  Missing expected argument: $arg"
        else
            log "  ✓ $arg"
        fi
    done
}

# ── Validate oracle-verifier initialization ──────────────────────────────────

validate_args "parashield-oracle-verifier" "initialize" \
    "admin"

# ── Validate policy-engine initialization ─────────────────────────────────────

validate_args "parashield-policy-engine" "initialize" \
    "admin" \
    "usdc_token" \
    "oracle_address"

# ── Validate risk-pool initialization ─────────────────────────────────────────

validate_args "parashield-risk-pool" "initialize" \
    "admin"

# ── Validate claims-processor initialization ──────────────────────────────────

validate_args "parashield-claims-processor" "initialize" \
    "admin" \
    "policy_engine" \
    "risk_pool" \
    "oracle_verifier" \
    "staleness_threshold"

# ── Validate governance-dao initialization ────────────────────────────────────

validate_args "parashield-governance-dao" "initialize" \
    "admin" \
    "config"

# ── Validate set_claims_processor call ────────────────────────────────────────

validate_args "parashield-policy-engine" "set_claims_processor" \
    "admin" \
    "claims_processor"

# ── Summary ───────────────────────────────────────────────────────────────────

if [ $FAILED -eq 0 ]; then
    log ""
    log "✓ All deployment script arguments match contract signatures"
    exit 0
else
    log ""
    err "Validation failed. Deployment scripts may break with current contract signatures."
    exit 1
fi
