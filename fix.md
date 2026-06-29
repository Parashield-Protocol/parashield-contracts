governance-dao.rs: execute_proposal does not verify proposal exists
Repo Avatar
Parashield-Protocol/parashield-contracts
governance-dao.rs: proposal execution does not verify proposal exists — could execute phantom proposal

execute_proposal(proposal_id) does not check if the proposal was actually created. Executing a non-existent proposal_id would either panic or succeed silently depending on storage state.

Acceptance criteria:

Guard: load the proposal and panic if not found
Test: execute a proposal_id that was never created, expect error