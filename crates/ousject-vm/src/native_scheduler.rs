//! Single-core cooperative scheduler for the Native VM adapter.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use oms_types::ObjectId;

use crate::execution_core::{ProcessStatus, VmError};
use crate::native::NativeVirtualMachine;

const MAX_SLICE_TOKENS: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeScheduleReport {
    pub steps: u64,
    pub statuses: BTreeMap<ObjectId, ProcessStatus>,
}

/// Round-robin scheduler over durable Native Process Objects.
///
/// The queue is only a scheduling hint. Process status remains authoritative
/// in the OMS, so stale queue entries cannot make a Waiting or completed
/// Process runnable.
pub struct NativeCooperativeScheduler<'a> {
    vm: &'a NativeVirtualMachine,
    queue: VecDeque<ObjectId>,
    known: Vec<ObjectId>,
    slice_tokens: u64,
}

impl<'a> NativeCooperativeScheduler<'a> {
    #[must_use]
    pub const fn new(vm: &'a NativeVirtualMachine) -> Self {
        Self {
            vm,
            queue: VecDeque::new(),
            known: Vec::new(),
            slice_tokens: MAX_SLICE_TOKENS,
        }
    }

    /// Sets a bounded per-Process quantum. Values outside 1..=256 are clamped.
    pub fn set_slice_tokens(&mut self, slice_tokens: u64) {
        self.slice_tokens = slice_tokens.clamp(1, MAX_SLICE_TOKENS);
    }

    /// Adds a Process to the runnable queue if its durable state is Ready.
    ///
    /// # Errors
    ///
    /// Returns an error when the Process Object cannot be read or decoded.
    pub fn enqueue(&mut self, process: ObjectId) -> Result<(), VmError> {
        let state = self.vm.process_state(process)?;
        if !self.known.contains(&process) {
            self.known.push(process);
        }
        if matches!(state.status, ProcessStatus::Ready | ProcessStatus::Running)
            && !self.queue.contains(&process)
        {
            self.queue.push_back(process);
        }
        Ok(())
    }

    /// Runs round-robin slices until the queue is empty or the token budget is
    /// consumed. Waiting Processes remain durable and are not polled or woken.
    ///
    /// # Errors
    ///
    /// Returns an OMS/VM error. A token failure is persisted by the VM and the
    /// scheduler continues with other queued Processes.
    pub fn run(&mut self, token_budget: u64) -> Result<NativeScheduleReport, VmError> {
        let mut steps = 0;
        while steps < token_budget {
            let Some(process) = self.queue.pop_front() else {
                break;
            };
            let state = self.vm.process_state(process)?;
            if !matches!(state.status, ProcessStatus::Ready | ProcessStatus::Running) {
                continue;
            }

            let remaining = token_budget - steps;
            let quantum = self.slice_tokens.min(remaining);
            match self.vm.run_slice(process, quantum) {
                Ok(report) => {
                    steps += report.steps;
                    if matches!(report.status, ProcessStatus::Ready | ProcessStatus::Running) {
                        self.queue.push_back(process);
                    }
                }
                Err(error) => {
                    if self.vm.process_state(process)?.status != ProcessStatus::Failed {
                        return Err(error);
                    }
                    // Failure is terminal for this queue entry, but not for
                    // other independent Processes.
                }
            }
        }

        let mut statuses = BTreeMap::new();
        for process in &self.known {
            statuses.insert(*process, self.vm.process_state(*process)?.status);
        }
        Ok(NativeScheduleReport { steps, statuses })
    }
}

#[cfg(test)]
mod tests {
    use alloc::rc::Rc;
    use alloc::vec;
    use oms_runtime::{AccessContext, InMemoryObjectManager};
    use oms_types::{SYSTEM_SUBJECT, Value, seed_id_generator};
    use tf_format::{Program, Token};

    use super::NativeCooperativeScheduler;
    use crate::execution_core::ProcessStatus;
    use crate::native::NativeVirtualMachine;

    #[test]
    fn round_robin_runs_independent_processes_after_another_process_fails() {
        seed_id_generator(0x4e41_5449_5645).expect("initialize deterministic test IDs");
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager);
        let program = Program {
            tokens: vec![
                Token::Push(Value::Integer(0)),
                Token::Store("counter".into()),
                Token::Load("counter".into()),
                Token::Push(Value::Integer(1)),
                Token::Add,
                Token::Store("counter".into()),
                Token::Push(Value::Integer(17)),
                Token::Halt,
            ],
        };
        let process_a = vm.create_process(&program).expect("create Process A");
        let process_b = vm.create_process(&program).expect("create Process B");
        let failed = vm
            .create_process(&Program {
                tokens: vec![
                    Token::ObjectCall {
                        method: "missing_provider".into(),
                        arguments: 0,
                    },
                    Token::Halt,
                ],
            })
            .expect("create failing Process");

        let mut scheduler = NativeCooperativeScheduler::new(&vm);
        scheduler.set_slice_tokens(1);
        scheduler.enqueue(process_a).expect("enqueue A");
        scheduler.enqueue(failed).expect("enqueue failing Process");
        scheduler.enqueue(process_b).expect("enqueue B");
        scheduler.run(3).expect("run first round");
        assert!(vm.process_state(process_a).unwrap().token_position > 0);
        assert!(vm.process_state(process_b).unwrap().token_position > 0);
        assert_eq!(
            vm.process_state(failed).unwrap().status,
            ProcessStatus::Failed
        );

        let report = scheduler.run(100).expect("finish runnable Processes");
        assert_eq!(report.statuses[&process_a], ProcessStatus::Halted);
        assert_eq!(report.statuses[&process_b], ProcessStatus::Halted);
        assert_eq!(report.statuses[&failed], ProcessStatus::Failed);
        for process in [process_a, process_b] {
            let state = vm.process_state(process).unwrap();
            let counter = state
                .frames
                .last()
                .and_then(|frame| frame.locals.get("counter"))
                .expect("independent counter local");
            assert_eq!(
                vm.manager()
                    .value(AccessContext::new(SYSTEM_SUBJECT), *counter)
                    .unwrap(),
                Value::Integer(1)
            );
        }
    }
}
