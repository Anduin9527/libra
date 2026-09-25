//! Minimal provider-aware context budgeting retained for repository Memory.
//!
//! The former Code executor owned compaction, handoff, frame, and projection
//! modules. Those surfaces were removed in RC-23; Memory only needs the
//! deterministic allocator, budget contract, receipt store, and confidence
//! vocabulary below.

use std::fmt;

use serde::{Deserialize, Serialize};

pub mod allocator;
pub mod budget;
#[allow(dead_code)]
pub(crate) mod memory;
#[allow(dead_code)]
pub(crate) mod receipt;
#[allow(dead_code)]
pub(crate) mod receipt_store;

pub use allocator::{
    AllocationOmissionReason, ContextAllocation, ContextAllocationOmission, ContextBudgetAllocator,
    ContextBudgetCandidate,
};
pub use budget::{
    ContextBudget, ContextBudgetError, ContextPriority, ContextSegmentBudget, ContextSegmentKind,
    ProviderContextCapability, SAFETY_MARGIN_TOKENS, TruncationPolicy,
};
/// Confidence attached to an admitted Memory claim.
///
/// This type used to live in the Code executor's reviewed-anchor module. It
/// remains a small shared value type because persisted Memory v1 payloads use
/// the same wire labels.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAnchorConfidence {
    Low,
    Medium,
    High,
}

impl fmt::Display for MemoryAnchorConfidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        })
    }
}
