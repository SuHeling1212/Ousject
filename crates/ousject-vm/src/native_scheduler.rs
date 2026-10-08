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

    /// Rebuilds the volatile queue from durable Native Process Objects.
    ///
    /// This also converts interrupted `Running` records back to `Ready` via
    /// the VM's atomic OMS recovery transition.
    ///
    /// # Errors
    ///
    /// Returns an OMS error or a malformed Process state error.
    pub fn recover_ready_processes(&mut self) -> Result<usize, VmError> {
        let processes = self.vm.recover_ready_processes()?;
        let count = processes.len();
        for process in processes {
            self.enqueue(process)?;
        }
        Ok(count)
    }

    /// Polls waiting Native Providers once, then queues newly Ready Processes.
    ///
    /// The caller chooses when to poll (normally after a timer interrupt), so
    /// a blocked input request never spins inside [`Self::run`].
    ///
    /// # Errors
    ///
    /// Returns an OMS, Provider, or Process-state error.
    pub fn poll_waiting_providers(&mut self) -> Result<usize, VmError> {
        let awakened = self.vm.poll_waiting_providers()?;
        let count = awakened.len();
        for process in awakened {
            self.enqueue(process)?;
        }
        Ok(count)
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
                    if matches!(
                        report.status,
                        ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
                    ) {
                        for waiter in self.vm.wake_process_waiters(process)? {
                            self.enqueue(waiter)?;
                        }
                    }
                    if matches!(report.status, ProcessStatus::Ready | ProcessStatus::Running) {
                        self.queue.push_back(process);
                    }
                }
                Err(error) => {
                    if self.vm.process_state(process)?.status != ProcessStatus::Failed {
                        return Err(error);
                    }
                    for waiter in self.vm.wake_process_waiters(process)? {
                        self.enqueue(waiter)?;
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
    use alloc::string::ToString;
    use alloc::vec;
    use oms_runtime::{AccessContext, InMemoryObjectManager};
    use oms_types::{SYSTEM_SUBJECT, Value, seed_id_generator};
    use tf_format::{Program, Token};

    use super::NativeCooperativeScheduler;
    use crate::execution_core::ProcessStatus;
    use crate::native::NativeVirtualMachine;

    #[test]
    fn round_robin_runs_independent_processes_after_another_process_fails() {
        let _ = seed_id_generator(0x4e41_5449_5645);
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

    #[test]
    fn process_wait_yields_and_resumes_after_child_halts() {
        let _ = seed_id_generator(0x5741_4954_4552);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager);
        let child = vm
            .create_process(&Program {
                tokens: vec![Token::Push(Value::Integer(23)), Token::Halt],
            })
            .expect("create child");
        let parent = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Text(child.to_string())),
                    Token::ObjectCall {
                        method: "wait".into(),
                        arguments: 0,
                    },
                    Token::Halt,
                ],
            })
            .expect("create parent");

        let mut scheduler = NativeCooperativeScheduler::new(&vm);
        scheduler.set_slice_tokens(1);
        scheduler.enqueue(parent).expect("enqueue parent");
        scheduler.enqueue(child).expect("enqueue child");
        scheduler.run(3).expect("parent should wait for child");
        let waiting = vm.process_state(parent).expect("read waiting parent");
        assert_eq!(waiting.status, ProcessStatus::Waiting);
        assert_eq!(
            waiting.wait_reason,
            crate::execution_core::WaitReason::Process(child)
        );
        let context = AccessContext::new(SYSTEM_SUBJECT);
        assert_eq!(
            vm.manager()
                .read(context, parent)
                .expect("read parent links")
                .links()
                .get("$waiting_on"),
            Some(&child)
        );
        assert_eq!(
            vm.manager()
                .read(context, child)
                .expect("read child links")
                .links()
                .get(&alloc::format!("$wait:{parent}")),
            Some(&parent)
        );

        let report = scheduler.run(20).expect("wake and finish both processes");
        assert_eq!(report.statuses[&parent], ProcessStatus::Halted);
        assert_eq!(report.statuses[&child], ProcessStatus::Halted);
        assert_eq!(
            vm.process_state(parent)
                .expect("read completed parent")
                .result,
            Some(Value::Text("halted".into()))
        );
        assert!(
            !vm.manager()
                .read(context, parent)
                .expect("read resumed parent links")
                .links()
                .contains_key("$waiting_on")
        );
        assert!(
            !vm.manager()
                .read(context, child)
                .expect("read completed child links")
                .links()
                .contains_key(&alloc::format!("$wait:{parent}"))
        );
        assert_eq!(
            vm.process_state(child)
                .expect("read completed child")
                .result,
            Some(Value::Integer(23))
        );
    }

    #[test]
    fn recovery_wakes_a_process_waiter_when_its_child_finished_while_offline() {
        let _ = seed_id_generator(0x5253_5441_5254);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager);
        let child = vm
            .create_process(&Program {
                tokens: vec![Token::Push(Value::Integer(23)), Token::Halt],
            })
            .expect("create child");
        let parent = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Text(child.to_string())),
                    Token::ObjectCall {
                        method: "wait".into(),
                        arguments: 0,
                    },
                    Token::Halt,
                ],
            })
            .expect("create parent");
        let mut first_scheduler = NativeCooperativeScheduler::new(&vm);
        first_scheduler.set_slice_tokens(1);
        first_scheduler.enqueue(parent).expect("enqueue parent");
        first_scheduler.run(2).expect("parent enters wait");
        assert_eq!(
            vm.process_state(parent)
                .expect("read waiting parent")
                .status,
            ProcessStatus::Waiting
        );
        vm.run_slice(child, 2)
            .expect("finish child while scheduler is offline");

        let mut recovered_scheduler = NativeCooperativeScheduler::new(&vm);
        assert_eq!(
            recovered_scheduler
                .recover_ready_processes()
                .expect("recover ready and dependent waiting Processes"),
            1
        );
        let report = recovered_scheduler.run(4).expect("resume recovered parent");
        assert_eq!(report.statuses[&parent], ProcessStatus::Halted);
        assert_eq!(
            vm.process_state(parent)
                .expect("read recovered parent")
                .result,
            Some(Value::Text("halted".into()))
        );
        assert!(
            !vm.manager()
                .read(AccessContext::new(SYSTEM_SUBJECT), parent)
                .expect("read recovered parent links")
                .links()
                .contains_key("$waiting_on")
        );
    }
}
