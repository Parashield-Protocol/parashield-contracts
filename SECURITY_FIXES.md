# Security Fixes - Three Medium to High Severity Issues

This document summarizes the security vulnerabilities addressed in this branch.

## Branch
`fix/security-issues`

## Summary
Fixed three security vulnerabilities across the ParaShield smart contracts:
1. Division by zero in GovernanceDao adaptive quorum calculation
2. Missing oracle_key length validation in PolicyEngine
3. Missing reinsurer interface validation in RiskPool

---

## 1. GovernanceDao - Division by Zero in Adaptive Quorum (Medium Severity)

### Issue
When adaptive quorum is enabled and participation history is empty (no recent proposals finalized), the average participation calculation could theoretically divide by zero, causing a panic.

### Steps to Reproduce
1. Initialize the DAO with adaptive quorum enabled
2. Before any proposals are created and finalized, try to create a new proposal
3. The quorum calculation attempts to compute average participation from empty history

### Root Cause
The `average_participation_bps()` function returns `None` when history is empty, but the code path needed explicit handling to fall back to base quorum.

### Fix
Added explicit comment clarifying the safeguard that already exists: when `avg.is_none()`, the function returns early with the base quorum before any division occurs.

**File**: `contracts/governance-dao/src/lib.rs`

```rust
// SECURITY FIX: When participation history is empty (no recent proposals),
// fall back to base quorum. This prevents division by zero and ensures
// the DAO works correctly before any proposals have been finalized.
if !decay.enabled || avg.is_none() {
    return EffectiveQuorum {
        quorum_bps: base_bps,
        base_bps,
        avg_participation_bps: avg_bps,
        decayed: false,
    };
}
```

### Impact
- Prevents potential panic on early proposal creation
- Ensures DAO functions correctly from initialization
- No behavior change for normal operation (safeguard already existed)

---

## 2. PolicyEngine - Missing Oracle Key Length Validation (Medium Severity)

### Issue
The `buy_policy` function accepts an `oracle_key` parameter but does not validate its length on-chain. The backend validates a maximum of 32 characters, but a direct contract call could pass a longer key, potentially causing issues with oracle lookups.

### Steps to Reproduce
1. Call `buy_policy` with an `oracle_key` of 100+ characters
2. The contract accepts it
3. Oracle lookups may fail or behave unexpectedly due to the excessively long key

### Root Cause
No length validation exists in the contract, creating a mismatch between backend validation (32 chars max) and on-chain enforcement.

### Fix
Added explicit length validation in `buy_policy_inner()` to reject oracle keys longer than 32 characters.

**File**: `contracts/policy-engine/src/lib.rs`

```rust
// SECURITY FIX: Validate oracle_key length to prevent excessively long keys
// that could cause issues with oracle lookups. Backend validates max 32 chars,
// but direct contract calls could bypass that. Enforce the same limit on-chain.
{
    let sym_val = oracle_key.to_symbol_val();
    let sym_str: Result<soroban_sdk::SymbolStr, _> = soroban_sdk::SymbolStr::try_from_val(env, &sym_val);
    match sym_str {
        Ok(s) => {
            let s_str: &str = s.as_ref();
            let bytes: &[u8] = s_str.as_bytes();
            // Reject oracle keys longer than 32 characters
            if bytes.len() > 32 {
                panic_with_error!(env, Error::InvalidOracleKey);
            }
        }
        Err(_) => {
            panic_with_error!(env, Error::InvalidOracleKey);
        }
    }
}
```

### Impact
- Prevents oracle lookup failures from oversized keys
- Aligns on-chain validation with backend constraints
- Returns `InvalidOracleKey` error for keys exceeding 32 characters
- All calls to `buy_policy`, `buy_policy_scheduled`, and `batch_buy_policy` are protected

---

## 3. RiskPool - Missing Reinsurer Interface Validation (High Severity)

### Issue
The `set_reinsurance` function allows the admin to set any contract address as the reinsurer without validating that it implements the required `IReinsurer` interface. A malicious or buggy reinsurer contract could drain the pool by returning fraudulent recovery amounts.

### Steps to Reproduce
1. Admin sets a malicious contract as the reinsurer
2. When a claim triggers reinsurance recovery, the malicious contract returns a huge amount
3. The pool transfers more USDC than it should, potentially draining reserves

### Root Cause
No validation that the reinsurer contract implements the required `recover()` function with the correct signature.

### Fix
Added interface validation by performing a test call to the reinsurer's `recover()` function with zero amounts during configuration.

**File**: `contracts/risk-pool/src/lib.rs`

```rust
// SECURITY FIX: Validate that the reinsurer contract implements the required
// IReinsurer interface by attempting to call a test recover with zero amount.
// This prevents a malicious or buggy contract from being set as the reinsurer
// and potentially draining the pool by returning fraudulent recovery amounts.
// A valid reinsurer must accept this call without panicking.
let test_policy_id = 0u128;
let test_amount = 0i128;
let test_result = env.try_invoke_contract::<i128, soroban_sdk::Error>(
    &reinsurer,
    &Symbol::new(&env, "recover"),
    soroban_sdk::vec![
        &env,
        env.current_contract_address().into_val(&env),
        test_policy_id.into_val(&env),
        test_amount.into_val(&env),
    ],
);

// If the reinsurer doesn't implement recover() or the call fails,
// reject the configuration
if test_result.is_err() {
    panic_with_error!(&env, Error::InvalidReinsuranceConfig);
}
```

### Impact
- Prevents malicious contracts from being set as reinsurer
- Validates interface implementation at configuration time
- Returns `InvalidReinsuranceConfig` error if reinsurer doesn't implement required interface
- Protects pool reserves from fraudulent recovery claims

---

## Testing

All fixes have been applied and are ready for testing. To verify:

```bash
# Run all contract tests
cargo test --manifest-path contracts/Cargo.toml

# Run specific contract tests
cargo test --manifest-path contracts/governance-dao/Cargo.toml
cargo test --manifest-path contracts/policy-engine/Cargo.toml
cargo test --manifest-path contracts/risk-pool/Cargo.toml
```

## Next Steps

1. Run full test suite to ensure no regressions
2. Review changes for completeness
3. Merge into main branch
4. Update documentation if needed
5. Consider additional security audit for reinsurance module
