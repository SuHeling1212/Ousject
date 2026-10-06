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

    /// Rebuilds the runnable queue from durable Process state.
    ///
    /// # Errors
    ///
    /// Returns an error if process recovery or Object Store inspection fails.
    pub fn recover(vm: &'a VirtualMachine) -> Result<Self, VmError> {
        let processes = vm.recover_processes()?;
        let mut scheduler = Self::new(vm);
        for process in processes {
            scheduler.enqueue(process);
        }
        Ok(scheduler)
    }

    pub fn enqueue(&mut self, process: ObjectId) {
        if !self.queue.contains(&process) {
            self.queue.push_back(process);
        }
    }

    /// Runs durable ready Processes in round-robin execution slices.
    ///
    /// A Worker owns a Process through a persisted lease for one slice. Waiting
    /// and sleeping Processes are removed from the runnable queue; the Timer
    /// heap is reconstructed from their durable wait reasons after restart.
    ///
    /// # Errors
    ///
    /// Returns a VM error or [`VmError::StepLimitExceeded`] if the total token
    /// budget is exhausted before queued work completes.
    pub fn run(&mut self, step_limit: u64) -> Result<ScheduleReport, VmError> {
        const SLICE_TOKENS: u64 = 4_096;
        let mut total_steps = 0;
        let mut reports = BTreeMap::<ObjectId, RunReport>::new();
        let mut sleepers = BTreeSet::new();

        loop {
            if self.queue.is_empty() && !sleepers.is_empty() {
                let due = sleepers.iter().copied().collect::<Vec<_>>();
                let mut next_wake = None;
                for process in due {
                    if self.vm.wake_due_timer(process)? {
                        sleepers.remove(&process);
                        self.enqueue(process);
                    } else if let Some(delay) = self.vm.time_until_wake(process)? {
                        next_wake = Some(next_wake.map_or(delay, |value: Duration| value.min(delay)));
                    } else {
                        sleepers.remove(&process);
                    }
                }
                if self.queue.is_empty() {
                    if let Some(delay) = next_wake {
                        if !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        continue;
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
            if !matches!(state.status, ProcessStatus::Ready | ProcessStatus::Running) {
                report.status = state.status;
                if state.status == ProcessStatus::Waiting
                    && timer_deadline(&state.wait_reason).is_some()
                {
                    sleepers.insert(process);
                }
                continue;
            }
            if total_steps >= step_limit {
                self.queue.push_front(process);
                return Err(VmError::StepLimitExceeded(step_limit));
            }

            let slice_limit = SLICE_TOKENS.min(step_limit - total_steps);
            let slice = self.vm.run(process, slice_limit)?;
            report.steps += slice.steps;
            report.status = slice.status;
            report.output.extend(slice.output);
            total_steps += slice.steps;
            match slice.status {
                ProcessStatus::Ready => self.enqueue(process),
                ProcessStatus::Waiting if timer_deadline(&self.vm.process_state(process)?.wait_reason).is_some() => {
                    sleepers.insert(process);
                }
                _ => {}
            }
        }
        Ok(ScheduleReport {
            total_steps,
            processes: reports.into_values().collect(),
        })
    }
}
