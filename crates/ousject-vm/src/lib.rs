//! Ousject TF virtual machine backed by Process and Variable Objects.

extern crate alloc;

mod execution_core;
mod machine;

pub use execution_core::{
    CallFrame, ExceptionHandler, ProcessState, ProcessStatus, VmError, WaitReason, WorkerLease,
};

pub use machine::{
    CooperativeScheduler, EffectRecoveryPolicy, EffectStatus, INSTANCE_TYPE, PROCESS_TYPE,
    PROGRAM_TYPE, ProcessReaper, RunReport, SYSTEM_SUBJECT, ScheduleReport, TerminalProvider,
    VirtualMachine,
};
