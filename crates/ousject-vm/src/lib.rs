//! Ousject TF virtual machine backed by Process and Variable Objects.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(all(feature = "std", feature = "native"))]
compile_error!("choose either Hosted `std` or the Native VM feature");
#[cfg(not(any(feature = "std", feature = "native")))]
compile_error!("enable either the Hosted `std` feature or the Native VM feature");

mod execution_core;

#[cfg(feature = "std")]
mod machine;
#[cfg(feature = "native")]
mod native;

pub use execution_core::{
    CallFrame, ExceptionHandler, ProcessState, ProcessStatus, VmError, WaitReason, WorkerLease,
};

#[cfg(feature = "std")]
pub use machine::{
    CooperativeScheduler, EffectRecoveryPolicy, EffectStatus, INSTANCE_TYPE, PROCESS_TYPE,
    PROGRAM_TYPE, ProcessReaper, RunReport, SYSTEM_SUBJECT, ScheduleReport, TerminalProvider,
    VirtualMachine,
};

#[cfg(feature = "native")]
pub use native::{NativeRunReport, NativeVirtualMachine, PROCESS_TYPE, PROGRAM_TYPE};
