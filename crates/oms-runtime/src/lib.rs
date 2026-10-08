//! Object Management System core.
//!
//! Transactions lock participating state in a stable global order, validate a
//! complete candidate, make it durable, and only then publish it. Readers can
//! therefore never observe a partially published cross-shard transaction.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod runtime;

pub use runtime::{
    AccessContext, CommitResult, CreateObject, CreateSpec, CreationPolicy, GcAnalysis, GcReport,
    InMemoryObjectManager, ObjectManager, ObjectQuery, ObjectView, OmsPerformanceStats, OmsStats,
    SnapshotBackend, SnapshotRecovery, StorageUsage, Transaction, TypeDescriptor, ValueSchema,
};
#[cfg(not(feature = "std"))]
pub use runtime::{BlockDevice, BlockSnapshotBackend};

#[cfg(feature = "std")]
pub use runtime::{FileSnapshotBackend, TombstoneReaper};
