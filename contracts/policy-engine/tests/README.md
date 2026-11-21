# Policy Engine Test Coverage

## Script Validation Tests

### test_create_products_json.rs

This test module validates that the JSON payloads in `scripts/create_products.sh` match the `CreateProductParams` struct definition.

**Purpose:** Catch mismatches between the deployment script and the contract struct at build time, preventing deployment failures.

**How it works:**
- Each product defined in `create_products.sh` has a corresponding test function
- The test manually constructs a `CreateProductParams` instance with the same values as the script's JSON
- If `CreateProductParams` gains or removes fields, the test will fail to compile
- If field types change, the test will fail to compile

**Coverage:**
1. Crop Drought Insurance (`test_crop_drought_insurance_json`)
2. Flight Delay Insurance (`test_flight_delay_insurance_json`)
3. DeFi Protocol Hack Insurance (`test_defi_hack_insurance_json`)
4. Natural Disaster Insurance (`test_natural_disaster_insurance_json`)
5. All-fields-required check (`test_all_fields_required`)

**When to update:**
- If you modify `CreateProductParams`, update both the test and `create_products.sh`
- If you add/remove products in `create_products.sh`, add/remove corresponding test functions
- The test will fail to compile if field names, types, or count changes

**Running the tests:**
```bash
cd contracts
cargo test --package parashield-policy-engine test_create_products_json
```

**Example failure scenario:**
If a developer adds a new field `pub risk_score: u32` to `CreateProductParams` but forgets to update `create_products.sh`, the test will fail to compile with:

```
error[E0063]: missing field `risk_score` in initializer of `types::CreateProductParams`
```

This forces the developer to update the script before the code can be deployed.
