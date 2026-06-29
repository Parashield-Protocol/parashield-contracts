oracle-verifier.rs: no TTL on data points — arbitrarily old data used for triggers
Repo Avatar
Parashield-Protocol/parashield-contracts
oracle-verifier.rs: get_latest_submission might return stale data silently — no TTL enforcement

Data is stored with a timestamp, but when verify_trigger queries it, there's no check that the data is recent. A 6-month-old rainfall submission could still be used to trigger claims.

Acceptance criteria:

Add MAX_DATA_AGE constant (e.g., 7 days)
In verify_trigger, check: current_time - data_timestamp <= MAX_DATA_AGE, else return error
Test: submit data, wait 8 days, call verify_trigger, expect error