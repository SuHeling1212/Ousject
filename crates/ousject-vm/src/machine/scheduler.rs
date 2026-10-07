const EFFECT_POLL_INTERVAL: Duration = Duration::from_millis(50);

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

    /// Drives the dependency tree of a Process wait until its waiter becomes
    /// runnable or the dependency is itself blocked on an external condition.
    /// Every Process is run in its own lease-bounded slice.
    #[allow(clippy::too_many_lines)]
    fn run_process_wait(&mut self, waiter: ObjectId, child: ObjectId) -> Result<(), VmError> {
        let mut tracked = BTreeSet::from([child]);
        let mut notified = BTreeSet::new();
        let mut interrupt_watches = BTreeMap::<ObjectId, ObjectId>::new();
        self.enqueue(child);

        let outcome = (|| loop {
            let waiter_state = self.vm.process_state(waiter)?;
            if waiter_state.status != ProcessStatus::Waiting
                || waiter_state.wait_reason != WaitReason::Process(child)
            {
                return Ok(());
            }

            let current = tracked.iter().copied().collect::<Vec<_>>();
            for process in current {
                let state = self.vm.process_state(process)?;
                if matches!(
                    state.status,
                    ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
                ) {
                    if notified.insert(process) {
                        self.vm.wake_process_waiters(process)?;
                    }
                    continue;
                }

                if state.status == ProcessStatus::Ready {
                    self.enqueue(process);
                } else if state.status == ProcessStatus::Waiting {
                    match state.wait_reason {
                        WaitReason::Process(dependency) => {
                            tracked.insert(dependency);
                            self.enqueue(dependency);
                        }
                        WaitReason::Timer { .. } => {
                            if self.vm.wake_due_timer(process)? {
                                self.enqueue(process);
                            }
                        }
                        WaitReason::Input(_) | WaitReason::Effect(_) => {
                            if self.vm.poll_pending_effect(process)? {
                                std::thread::sleep(EFFECT_POLL_INTERVAL);
                                self.enqueue(process);
                            }
                        }
                        WaitReason::Ipc(_) => {
                            // A Process.wait caller is a cooperative driver.
                            // Let it resume with "suspended" when its child
                            // reaches an external IPC wait, so the caller can
                            // perform the send/receive that makes progress.
                            self.vm.wake_process_waiters(process)?;
                        }
                        WaitReason::None => {}
                    }
                }

                if let std::collections::btree_map::Entry::Vacant(entry) =
                    interrupt_watches.entry(process)
                {
                    let parent = self.vm.manager.inspect(
                        AccessContext::new(SYSTEM_SUBJECT),
                        process,
                    )?.parent_id;
                    let terminal = parent.filter(|parent| {
                        self.vm
                            .manager
                            .inspect(AccessContext::new(SYSTEM_SUBJECT), *parent)
                            .is_ok_and(|header| {
                                header.type_id == CORE_TERMINAL_TYPE
                            })
                    });
                    if let (Some(terminal), Some(driver)) = (terminal, &self.vm.terminal_driver) {
                        if driver.is_interactive() {
                            driver
                                .begin_interrupt_watch(process)
                                .map_err(VmError::Provider)?;
                            entry.insert(terminal);
                        }
                    }
                }
            }

            if let Some(process) = self.queue.pop_front() {
                let state = self.vm.process_state(process)?;
                if matches!(state.status, ProcessStatus::Ready | ProcessStatus::Running) {
                    let report = match self.vm.run_slice(process, 4_096) {
                        Ok(report) => report,
                        Err(_error)
                            if self.vm.process_state(process)?.status == ProcessStatus::Failed =>
                        {
                            // A failed dependency is a completed wait target.
                            // Its waiter must resume so Process.wait() can
                            // return "failed" and the caller can inspect the
                            // child's persisted error.
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    tracked.insert(process);
                    if report.steps == 0
                        && matches!(report.status, ProcessStatus::Ready | ProcessStatus::Running)
                    {
                        // Another Worker owns this Process. Leave the durable
                        // waiter in place for that Worker to wake.
                        return Ok(());
                    }
                }

                // Poll after giving a Ready process an execution slice. An
                // interrupt is intended to cancel active work, not consume a
                // pending Ctrl-C before a newly submitted command has begun.
                if let Some(driver) = &self.vm.terminal_driver {
                    for (watched, terminal) in &interrupt_watches {
                        if driver
                            .take_interrupt(*watched)
                            .map_err(VmError::Provider)?
                        {
                            let state = self.vm.process_state(*watched)?;
                            self.vm.interrupt_terminal(*terminal, state.subject)?;
                            notified.insert(*watched);
                        }
                    }
                }
                continue;
            }

            // Timers are durable scheduler waits; sleep only until the
            // earliest tracked Process becomes runnable.
            let mut next_wake = None;
            for process in &tracked {
                if let Some(delay) = self.vm.time_until_wake(*process)? {
                    next_wake = Some(next_wake.map_or(delay, |value: Duration| value.min(delay)));
                }
            }
            if let Some(delay) = next_wake {
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                continue;
            }

            // No runnable or time-based work remains. The parent stays
            // durably Waiting until an external event or another scheduler
            // advances the dependency.
            return Ok(());
        })();

        if let Some(driver) = &self.vm.terminal_driver {
            for process in interrupt_watches.keys() {
                driver.end_interrupt_watch(*process);
            }
        }
        outcome
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
    #[allow(clippy::too_many_lines)]
    pub fn run(&mut self, step_limit: u64) -> Result<ScheduleReport, VmError> {
        const SLICE_TOKENS: u64 = 4_096;
        let mut total_steps = 0;
        let mut reports = BTreeMap::<ObjectId, RunReport>::new();
        let mut sleepers = BTreeSet::new();
        let mut waiting_on_process = BTreeMap::<ObjectId, ObjectId>::new();
        let mut dependencies = BTreeSet::new();
        let mut notified = BTreeSet::new();
        let system = AccessContext::new(SYSTEM_SUBJECT);
        for child in self.queue.iter().copied().collect::<Vec<_>>() {
            if let Ok(view) = self.vm.manager.read(system, child) {
                for waiter in view
                    .links()
                    .iter()
                    .filter(|(name, _)| name.starts_with("$wait:"))
                    .map(|(_, waiter)| waiter)
                {
                    waiting_on_process.insert(*waiter, child);
                    dependencies.insert(child);
                }
            }
        }

        loop {
            let waiters = waiting_on_process
                .iter()
                .map(|(waiter, child)| (*waiter, *child))
                .collect::<Vec<_>>();
            for (waiter, child) in waiters {
                let state = self.vm.process_state(waiter)?;
                if state.status == ProcessStatus::Waiting
                    && state.wait_reason == WaitReason::Process(child)
                {
                    dependencies.insert(child);
                    self.enqueue(child);
                } else {
                    waiting_on_process.remove(&waiter);
                    if state.status == ProcessStatus::Ready {
                        self.enqueue(waiter);
                    }
                }
            }

            let tracked = dependencies.iter().copied().collect::<Vec<_>>();
            for process in tracked {
                if let Ok(view) = self.vm.manager.read(system, process) {
                    for waiter in view
                        .links()
                        .iter()
                        .filter(|(name, _)| name.starts_with("$wait:"))
                        .map(|(_, waiter)| waiter)
                    {
                        waiting_on_process.insert(*waiter, process);
                    }
                }
                let state = self.vm.process_state(process)?;
                match state.status {
                    ProcessStatus::Ready => self.enqueue(process),
                    ProcessStatus::Waiting => match state.wait_reason {
                        WaitReason::Process(child) => {
                            dependencies.insert(child);
                            self.enqueue(child);
                        }
                        WaitReason::Timer { .. } => {
                            if self.vm.wake_due_timer(process)? {
                                sleepers.remove(&process);
                                self.enqueue(process);
                            } else {
                                sleepers.insert(process);
                            }
                        }
                        WaitReason::Input(_) | WaitReason::Effect(_) => {
                            if self.vm.poll_pending_effect(process)? {
                                std::thread::sleep(EFFECT_POLL_INTERVAL);
                                self.enqueue(process);
                            }
                        }
                        WaitReason::Ipc(_) => {
                            if notified.insert(process) {
                                self.vm.wake_process_waiters(process)?;
                            }
                        }
                        WaitReason::None => {}
                    },
                    ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed => {
                        sleepers.remove(&process);
                        if notified.insert(process) {
                            self.vm.wake_process_waiters(process)?;
                        }
                    }
                    ProcessStatus::Running | ProcessStatus::Suspended => {}
                }
            }

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
                if state.status == ProcessStatus::Waiting {
                    if let WaitReason::Process(child) = state.wait_reason {
                        waiting_on_process.insert(process, child);
                        dependencies.insert(child);
                        self.enqueue(child);
                    } else if timer_deadline(&state.wait_reason).is_some() {
                        sleepers.insert(process);
                    }
                }
                continue;
            }
            if total_steps >= step_limit {
                self.queue.push_front(process);
                return Err(VmError::StepLimitExceeded(step_limit));
            }

            let slice_limit = SLICE_TOKENS.min(step_limit - total_steps);
            let slice = match self.vm.run_slice(process, slice_limit) {
                Ok(slice) => slice,
                Err(error) => {
                    if dependencies.contains(&process)
                        && self.vm.process_state(process)?.status == ProcessStatus::Failed
                    {
                        report.status = ProcessStatus::Failed;
                        if notified.insert(process) {
                            self.vm.wake_process_waiters(process)?;
                        }
                        continue;
                    }
                    return Err(error);
                }
            };
            report.steps += slice.steps;
            report.status = slice.status;
            report.output.extend(slice.output);
            total_steps += slice.steps;
            match slice.status {
                ProcessStatus::Ready => self.enqueue(process),
                ProcessStatus::Waiting => {
                    let state = self.vm.process_state(process)?;
                    if let WaitReason::Process(child) = state.wait_reason {
                        waiting_on_process.insert(process, child);
                        dependencies.insert(child);
                        self.enqueue(child);
                    } else if timer_deadline(&state.wait_reason).is_some() {
                        sleepers.insert(process);
                    }
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
