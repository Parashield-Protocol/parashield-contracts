claims-processor.rs: no authorization checks on submit_claim / auto_process
Repo Avatar
Parashield-Protocol/parashield-contracts
claims-processor.rs: no check that caller is authorized — any address can submit or process claims

submit_claim and auto_process do not verify that the caller is an authorized keeper or the policyholder. Anyone can submit claims for anyone else's policies.

Acceptance criteria:

submit_claim: verify caller is the policyholder (from policy.policyholder field)
auto_process: verify caller is an authorized keeper or operator (stored in contract)
Test: call submit_claim for someone else's policy, expect unauthorized error