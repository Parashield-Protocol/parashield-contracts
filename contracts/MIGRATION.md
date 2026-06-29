# Contract Versioning and Migration Strategy

## Overview

All Parashield contracts now implement a versioning system that tracks storage schema versions and provides a framework for safe storage migrations during upgrades.

## Version Storage

Each contract stores its current version number in contract instance storage under the `StorageKey::Version` key. The initial version is `1` (default when no version is stored).

## Version Query

Each contract exposes a `get_version()` function that returns the current storage schema version as a `u32`:

```rust
pub fn get_version(env: Env) -> u32 {
    env.storage().instance().get(&StorageKey::Version).unwrap_or(1)
}
```

## Upgrade Process

When upgrading a contract, the `upgrade()` function now requires a `new_version` parameter:

```rust
pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
    // 1. Verify admin authorization
    // 2. Check new_version > current_version
    // 3. Run migrations from current_version to new_version
    // 4. Update stored version to new_version
    // 5. Perform WASM upgrade
    // 6. Emit ContractUpgraded event
}
```

### Version Validation

- `new_version` must be strictly greater than `current_version`
- Attempting to upgrade to the same or lower version will panic

### Migration Execution

The `run_migrations()` function is called before the WASM upgrade. It iterates through version transitions and applies necessary storage changes:

```rust
fn run_migrations(env: &Env, old_version: u32, new_version: u32) {
    if old_version < 2 && new_version >= 2 {
        Self::migrate_v1_to_v2(env);
    }
    if old_version < 3 && new_version >= 3 {
        Self::migrate_v2_to_v3(env);
    }
    // ... additional migrations
}
```

## Adding a New Version

When introducing a new storage schema version:

1. **Bump the VERSION constant** in your contract (or document the version number)

2. **Add migration function** in `run_migrations()`:
   ```rust
   fn run_migrations(env: &Env, old_version: u32, new_version: u32) {
       if old_version < 2 && new_version >= 2 {
           Self::migrate_v1_to_v2(env);
       }
       // Add new migration here
       if old_version < 3 && new_version >= 3 {
           Self::migrate_v2_to_v3(env);
       }
   }
   ```

3. **Implement the migration function**:
   ```rust
   fn migrate_v2_to_v3(env: &Env) {
       // Example: Add new field to existing products
       // let product_ids = Self::get_all_product_ids(env);
       // for id in product_ids {
       //     let mut product = Self::load_product(env, id);
       //     product.new_field = default_value;
       //     env.storage().persistent().set(&StorageKey::Product(id), &product);
       // }
   }
   ```

4. **Update tests** to verify migration correctness

## Migration Examples

### v1 → v2 Migration (No changes needed)

The initial version has no migrations. This serves as the baseline.

### Future v2 → v3 Migration Example

If you need to add a new field to `InsuranceProduct`:

```rust
fn migrate_v2_to_v3(env: &Env) {
    // Get all active product IDs
    let active_products: Vec<u128> = env.storage().instance()
        .get(&StorageKey::ActiveProducts)
        .unwrap_or_else(|| Vec::new(env));
    
    // Migrate each product
    for i in 0..active_products.len() {
        let product_id = active_products.get_unchecked(i);
        let mut product: InsuranceProduct = env.storage().persistent()
            .get(&StorageKey::Product(product_id))
            .unwrap();
        
        // Add new field with default value
        product.new_field = default_value;
        
        // Save updated product
        env.storage().persistent().set(&StorageKey::Product(product_id), &product);
    }
}
```

## Events

Each upgrade emits a `ContractUpgraded` event:

```rust
pub struct ContractUpgraded {
    pub old_version: u32,
    pub new_version: u32,
}
```

This allows off-chain systems to track version changes and trigger any necessary external migrations.

## Testing

### Test Version Initialization

```rust
#[test]
fn test_initial_version_is_one() {
    let (env, _admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    assert_eq!(client.get_version(), 1);
}
```

### Test Version Upgrade

```rust
#[test]
fn test_upgrade_increments_version() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    
    assert_eq!(client.get_version(), 1);
    client.upgrade(&admin, &BytesN::from_array(&env, &[0u8; 32]), &2);
    assert_eq!(client.get_version(), 2);
}
```

### Test Version Validation

```rust
#[test]
#[should_panic(expected = "new version must be greater than current version")]
fn test_upgrade_to_same_version_panics() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    client.upgrade(&admin, &BytesN::from_array(&env, &[0u8; 32]), &1);
}
```

## Migration Path Documentation

For each version bump, document:

1. **Version Number**: The new version number
2. **Changes**: What storage schema changes are being made
3. **Migration Logic**: The specific migration function implementation
4. **Rollback Plan**: How to handle migration failures (if applicable)
5. **Testing**: How to verify the migration works correctly

## Best Practices

1. **Always test migrations** with realistic data volumes before mainnet deployment
2. **Keep migrations idempotent** - they should be safe to run multiple times
3. **Use events** to track migration progress for debugging
4. **Document every version change** in this file
5. **Consider gas limits** - very large migrations may need to be split across multiple transactions
6. **Back up storage** before running migrations in production

## Contract-Specific Notes

### PolicyEngine
- Products and policies are stored individually
- Migrations should iterate through all active products/policies

### ClaimsProcessor
- Claims are stored individually
- Migrations should iterate through all pending claims

### OracleVerifier
- Oracle data points are stored by (data_type, key)
- Migrations should iterate through all data points

### GovernanceDao
- Proposals are stored individually
- Migrations should iterate through all active proposals

### RiskPool
- LP positions are stored by provider address
- Migrations should iterate through all LP positions
