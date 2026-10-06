pub struct CooperativeScheduler<'a> {
    vm: &'a VirtualMachine,
    queue: VecDeque<ObjectId>,
}

impl<'a> CooperativeScheduler<'a> {
    #[must_use]
    pub const fn new(vm: &'a VirtualMachine) -> Self {
        Self {
            vm,
            queue: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, process: ObjectId) {
        if !self.queue.contains(&process) {
            self.queue.push_back(process);
        }
    }

    /// Runs queued Processes cooperatively, one Token per scheduling turn.
    ///
    /// # Errors
    ///
    /// Returns a VM error or [`VmError::StepLimitExceeded`] if not all queued
    /// Processes halt within the total step budget.
    pub fn run(&mut self, step_limit: u64) -> Result<ScheduleReport, VmError> {
        let mut total_steps = 0;
        let mut reports = BTreeMap::<ObjectId, RunReport>::new();
        let mut sleepers = BTreeSet::new();
        loop {
            if self.queue.is_empty() && !sleepers.is_empty() {
                let mut next_wake = None;
                let candidates = sleepers.iter().copied().collect::<Vec<_>>();
                for process in candidates {
                    if self.vm.wake_due_timer(process)? {
                        sleepers.remove(&process);
                        self.enqueue(process);
                    } else {
                        let state = self.vm.process_state(process)?;
                        if state.status == ProcessStatus::Running {
                            sleepers.remove(&process);
                            self.enqueue(process);
                        } else if state.wake_at_unix_ms.is_none() {
                            sleepers.remove(&process);
                            if state.status == ProcessStatus::Running {
                                self.enqueue(process);
                            }
                        } else if let Some(delay) = self.vm.time_until_wake(process)? {
                            next_wake =
                                Some(next_wake.map_or(delay, |current: std::time::Duration| {
                                    current.min(delay)
                                }));
                        }
                    }
                }
                if self.queue.is_empty() {
                    if let Some(delay) = next_wake {
                        std::thread::sleep(delay);
                        continue;
                    }
                    if sleepers.is_empty() {
                        break;
                    }
                }
            }
            let Some(process) = self.queue.pop_front() else {
                break;
            };
            let state = self.vm.process_state(process)?;
            let report = reports.entry(process).or_insert_with(|| RunReport {
                process,
                steps: 0,
                status: state.status,
                output: Vec::new(),
            });
            if state.status != ProcessStatus::Running {
                report.status = state.status;
                continue;
            }
            if total_steps >= step_limit {
                return Err(VmError::StepLimitExceeded(step_limit));
            }
            let executor = self.vm.for_subject(state.subject);
            let (line, executed) = executor.step(process, state, step_limit - total_steps)?;
            if let Some(line) = line {
                report.output.push(line);
            }
            report.steps += executed;
            total_steps += executed;
            let next_state = self.vm.process_state(process)?;
            report.status = next_state.status;
            if next_state.status == ProcessStatus::Running {
                self.queue.push_back(process);
            } else if next_state.status == ProcessStatus::Suspended
                && next_state.wake_at_unix_ms.is_some()
            {
                sleepers.insert(process);
            }
        }
        Ok(ScheduleReport {
            total_steps,
            processes: reports.into_values().collect(),
        })
    }
}
