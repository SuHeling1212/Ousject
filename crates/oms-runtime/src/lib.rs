//! Object Management System core.
//!
//! Transactions lock participating state in a stable global order, validate a
//! complete candidate, make it durable, and only then publish it. Readers can
//! therefore never observe a partially published cross-shard transaction.

mod runtime;

pub use runtime::{
    AccessContext, CommitResult, CreateObject, CreateSpec, CreationPolicy, FileSnapshotBackend,
    GcAnalysis, GcReport, InMemoryObjectManager, ObjectManager, ObjectQuery, ObjectView,
    OmsPerformanceStats, OmsStats, SnapshotBackend, SnapshotRecovery, StorageUsage,
    TombstoneReaper, Transaction, TypeDescriptor, ValueSchema,
};
