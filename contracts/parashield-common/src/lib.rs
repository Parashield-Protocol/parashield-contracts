//! Parashield Common — shared protocol-wide constants
//!
//! This crate is the single source of truth for constants that must be
//! identical across all Parashield contracts. Previously each contract
//! defined its own copy with a comment reading "kept in sync by hand"
//! (issue #342). Centralising them here means a change in one place
//! propagates to every contract at compile time, and divergence becomes
//! a compile error rather than a silent runtime discrepancy.
//!
//! # Usage
//!
//! ```rust,ignore
//! use parashield_common::{TTL_THRESHOLD, TTL_EXTEND_TO, ADMIN_TRANSFER_TIMELOCK};
//! ```
#![no_std]

// ─── Storage TTL ──────────────────────────────────────────────────────────────

/// Extend a persistent entry's TTL once it has fewer than ~30 days of life
/// left (at ~5 s/ledger).
///
/// Used by every contract that writes to `storage().persistent()`. When
/// a persistent entry's remaining TTL falls below this value the contract
/// extends it on the next write, preventing silent storage eviction.
pub const TTL_THRESHOLD: u32 = 518_400; // ~30 days at 5 s/ledger

/// Extend persistent entries out to ~1 year (at ~5 s/ledger).
///
/// Paired with [`TTL_THRESHOLD`]: once the threshold is crossed the entry
/// is extended to this target, giving it roughly a year of life from the
/// moment it was last touched.
pub const TTL_EXTEND_TO: u32 = 6_312_000; // ~1 year at 5 s/ledger

// ─── Admin rotation ───────────────────────────────────────────────────────────

/// Grace period between an admin transfer being armed and the proposed
/// admin being able to call `accept_admin` (issue #356).
///
/// 48 hours gives protocol stakeholders a window to notice and respond to
/// a hostile or mistaken rotation before it takes effect. All four
/// contracts that expose admin rotation
/// (claims-processor, risk-pool, oracle-verifier, policy-engine)
/// must use this same value so the security guarantee is uniform.
pub const ADMIN_TRANSFER_TIMELOCK: u64 = 48 * 60 * 60; // 48 hours in seconds
