# Implementation Checklist

## ✅ Completed Changes

### 1. Governance DAO - Mandatory Impact Analysis
- [x] Added `impact_analysis: Bytes` field to `Proposal` struct
- [x] Updated `create_proposal()` function signature and validation
- [x] Updated `propose_upgrade()` function signature and validation
- [x] Added 4096 byte limit validation
- [x] Added non-empty validation
- **Status**: Types and library implementation complete

### 2. Oracle Verifier - Per-Product Consensus Threshold
- [x] Added `ConsensusThreshold` struct to types.rs
- [x] Added `ConsensusThresholdUpdated` event to types.rs
- [x] Added `StorageKey::ConsensusThreshold(Symbol)` variant
- [x] Implemented `set_consensus_threshold()` function
- [x] Implemented `get_consensus_threshold()` function with 5000 bps default
- [x] Basis points validation (0-10000)
- **Status**: Types and library implementation complete

### 3. Claims Processor - Installment Payout Option
- [x] Added `InstallmentSchedule` struct to types.rs
- [x] Added `installments: Option<InstallmentSchedule>` to `Claim` struct
- [x] Added `InstallmentPayoutScheduled` event to types.rs
- [x] Added `InstallmentPaid` event to types.rs
- [x] Implemented `schedule_installments()` function
- [x] Implemented `claim_installment()` function
- [x] Automatic installment availability calculation
- **Status**: Types and library implementation complete

### 4. Risk Pool - Dynamic Fee Adjustment
- [x] Added `DynamicFeeConfig` struct to types.rs
- [x] Added `DynamicFeeAdjusted` event to types.rs
- [x] Added `DynamicFeeConfigUpdated` event to types.rs
- [x] Added `StorageKey::DynamicFeeConfig` variant
- [x] Implemented `set_dynamic_fee_config()` function
- [x] Implemented `get_dynamic_fee_config()` function with defaults
- [x] Implemented `calculate_dynamic_fee()` function
- [x] Parameter validation (0-10000 basis points)
- **Status**: Types and library implementation complete

---

## 📋 Next Steps (For Testing & Integration)

### Governance DAO
- [ ] Add tests for impact_analysis validation
- [ ] Test proposal creation with empty impact_analysis (should fail)
- [ ] Test proposal creation with >4096 byte impact_analysis (should fail)
- [ ] Update test.rs and test_advanced.rs for new parameter
- [ ] Update proposal templates if applicable

### Oracle Verifier
- [ ] Add tests for consensus threshold setting
- [ ] Test default 5000 bps when not configured
- [ ] Test per-product configuration independence
- [ ] Integrate consensus threshold into vote verification logic
- [ ] Update oracle agreement validation to use per-product threshold

### Claims Processor
- [ ] Add tests for installment scheduling
- [ ] Test installment calculation logic
- [ ] Test claim_installment() availability calculation
- [ ] Test installment tracking and completion
- [ ] Add tests for edge cases (partial installments, etc.)

### Risk Pool
- [ ] Add tests for dynamic fee calculation
- [ ] Test fee capping at max/min bounds
- [ ] Test threshold-based fee increase logic
- [ ] Test disabled state behavior
- [ ] Integration tests: verify fees applied to premiums

---

## 🔧 Code Integration Notes

### Storage Keys
- All new storage keys have been added to respective enum definitions
- No key collisions or conflicts

### Events
- All new events follow existing naming conventions
- Events published with proper symbol keys

### Validation
- All numeric inputs validated against basis point ranges (0-10000)
- Length validations enforced where applicable
- Authorization checks maintained (admin/keeper only)

### Backward Compatibility
- New `Claim` field is `Option<InstallmentSchedule>` - optional
- New `Proposal` field is `Bytes` - required for new proposals
- Dynamic fee config defaults to disabled state
- Consensus threshold defaults to 5000 bps

---

## 📝 Notes

- Branch name: `feature/enhance-governance-oracle-claims-pool`
- All code follows existing Rust/Soroban contract patterns
- Type-safe implementations using Soroban SDK
- Event-driven architecture maintained
- Admin/auth patterns consistent with codebase

---

## Related Files Modified

```
contracts/governance-dao/src/lib.rs
contracts/governance-dao/src/types.rs
contracts/oracle-verifier/src/lib.rs
contracts/oracle-verifier/src/types.rs
contracts/claims-processor/src/lib.rs
contracts/claims-processor/src/types.rs
contracts/risk-pool/src/lib.rs
contracts/risk-pool/src/types.rs
```

---

## Quick Reference

### Governance DAO
```rust
// Proposal now requires impact_analysis
pub fn create_proposal(
    env: Env,
    proposer: Address,
    title: Bytes,
    target: Address,
    function: Symbol,
    args: Vec<Val>,
    impact_analysis: Bytes,  // NEW
) -> u64
```

### Oracle Verifier
```rust
// Per-product consensus threshold
pub fn set_consensus_threshold(
    env: Env,
    admin: Address,
    data_type: Symbol,
    agreement_threshold_bps: u32,  // 0-10000
)

pub fn get_consensus_threshold(env: Env, data_type: Symbol) -> ConsensusThreshold
```

### Claims Processor
```rust
// Installment payout scheduling
pub fn schedule_installments(
    env: Env,
    caller: Address,
    claim_id: u128,
    amount_per_installment: i128,
    num_installments: u32,
    interval_seconds: u64,
)

pub fn claim_installment(env: Env, claimant: Address, claim_id: u128) -> i128
```

### Risk Pool
```rust
// Dynamic fee configuration
pub fn set_dynamic_fee_config(
    env: Env,
    admin: Address,
    base_fee_bps: u32,
    max_fee_bps: u32,
    min_fee_bps: u32,
    utilization_threshold_bps: u32,
    fee_adjustment_per_1pct_bps: u32,
    enabled: bool,
)

pub fn calculate_dynamic_fee(env: Env) -> u32
```
