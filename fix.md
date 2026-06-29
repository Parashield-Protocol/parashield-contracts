risk-pool.rs: admin can drain pool — LP funds unprotected
Repo Avatar
Parashield-Protocol/parashield-contracts
risk-pool.rs: emergency drain by admin — admin can always withdraw from pool, leaving LPs with nothing

The admin might call withdraw_all or similar to drain the pool. There's no multi-sig or timelock protecting LPs.

Acceptance criteria:

Admin cannot withdraw LP funds directly
OR implement a 7-day timelock before withdrawal is allowed
Document: admin powers and limitations
Test: admin attempts to drain pool, verify LP funds are protected