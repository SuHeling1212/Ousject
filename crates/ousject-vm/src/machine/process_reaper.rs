fn reap_expired_processes(
    manager: &Arc<InMemoryObjectManager>,
    now: SystemTime,
) -> Result<(), VmError> {
    let now = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let cutoff = now.saturating_sub(PROCESS_RETENTION_MILLIS);
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let processes = manager.query(context, &ObjectQuery::new().with_type(PROCESS_TYPE))?;
    let mut retired = BTreeSet::new();
    let mut discovery_order = Vec::new();
    for header in processes {
        if retired.contains(&header.id) {
            continue;
        }
        let view = match manager.read(context, header.id) {
            Ok(view) => view,
            Err(OmsError::InvalidLifecycle { .. }) => continue,
            Err(error) => return Err(error.into()),
        };
        let state = decode_process_state(view.state())?;
        if !matches!(
            state.status,
            ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
        ) || state.ended_at_unix_ms.is_none_or(|ended| ended > cutoff)
        {
            continue;
        }
        let package_instance = view.header().parent_id.filter(|parent| {
            manager
                .inspect(context, *parent)
                .is_ok_and(|parent| parent.type_id == CORE_PACKAGE_INSTANCE_TYPE)
        });
        let cleanup_root = package_instance.unwrap_or(header.id);
        if let Some(tree) = collect_expired_process_tree(manager, cleanup_root, cutoff)? {
            for object in tree {
                if retired.insert(object) {
                    discovery_order.push(object);
                }
            }
        }
    }
    if retired.is_empty() {
        Ok(())
    } else {
        retire_process_objects(manager, &retired, &discovery_order)
    }
}

fn process_cleanup_deadline(
    manager: &InMemoryObjectManager,
    process: ObjectId,
) -> Result<Option<u64>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let header = manager.inspect(context, process)?;
    if header.type_id != PROCESS_TYPE || header.lifecycle == LifecycleState::Tombstoned {
        return Ok(None);
    }
    let state = decode_process_state(manager.read(context, process)?.state())?;
    if !matches!(
        state.status,
        ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
    ) {
        return Ok(None);
    }
    Ok(state
        .ended_at_unix_ms
        .map(|ended| ended.saturating_add(PROCESS_RETENTION_MILLIS)))
}

fn finished_process_deadlines(
    manager: &Arc<InMemoryObjectManager>,
) -> Result<BTreeSet<(u64, ObjectId)>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    manager
        .query(context, &ObjectQuery::new().with_type(PROCESS_TYPE))?
        .into_iter()
        .filter_map(
            |header| match process_cleanup_deadline(manager, header.id) {
                Ok(Some(deadline)) => Some(Ok((deadline, header.id))),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect()
}

fn process_ancestors(
    manager: &InMemoryObjectManager,
    process: ObjectId,
) -> Result<Vec<ObjectId>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut ancestors = Vec::new();
    let mut parent = manager.inspect(context, process)?.parent_id;
    while let Some(object) = parent {
        let header = manager.inspect(context, object)?;
        if header.type_id == PROCESS_TYPE {
            ancestors.push(object);
        }
        parent = header.parent_id;
    }
    Ok(ancestors)
}

fn schedule_finished_process(
    manager: &InMemoryObjectManager,
    process: ObjectId,
    indexed: &mut BTreeMap<ObjectId, u64>,
    deadlines: &mut BTreeSet<(u64, ObjectId)>,
) -> Result<(), VmError> {
    if let Some(previous) = indexed.remove(&process) {
        deadlines.remove(&(previous, process));
    }
    if let Some(deadline) = process_cleanup_deadline(manager, process)? {
        indexed.insert(process, deadline);
        deadlines.insert((deadline, process));
    }
    Ok(())
}

fn process_reaper_loop(
    manager: &Arc<InMemoryObjectManager>,
    initial_deadlines: BTreeSet<(u64, ObjectId)>,
    receiver: &mpsc::Receiver<ProcessReaperMessage>,
    interval: Duration,
) {
    let mut deadlines = initial_deadlines;
    let mut indexed = deadlines
        .iter()
        .map(|(deadline, process)| (*process, *deadline))
        .collect::<BTreeMap<_, _>>();
    let interval = interval.max(Duration::from_secs(1));
    loop {
        let now = unix_time_millis();
        let due = deadlines
            .iter()
            .take_while(|(deadline, _)| *deadline <= now)
            .copied()
            .collect::<Vec<_>>();
        if !due.is_empty() {
            let mut ancestors = BTreeSet::new();
            for (_, process) in &due {
                if let Ok(found) = process_ancestors(manager, *process) {
                    ancestors.extend(found);
                }
            }
            match reap_expired_processes(manager, SystemTime::now()) {
                Ok(()) => {
                    for (deadline, process) in due {
                        deadlines.remove(&(deadline, process));
                        indexed.remove(&process);
                    }
                }
                Err(error) => {
                    eprintln!("Ousject Process cleanup failed: {error}");
                    let retry_at =
                        now.saturating_add(interval.as_millis().try_into().unwrap_or(u64::MAX));
                    for (_, process) in due {
                        if let Some(previous) = indexed.insert(process, retry_at) {
                            deadlines.remove(&(previous, process));
                        }
                        deadlines.insert((retry_at, process));
                    }
                }
            }
            for ancestor in ancestors {
                if let Err(error) =
                    schedule_finished_process(manager, ancestor, &mut indexed, &mut deadlines)
                {
                    eprintln!("Ousject Process cleanup scheduling failed: {error}");
                }
            }
            continue;
        }

        let delay = deadlines
            .first()
            .map(|(deadline, _)| Duration::from_millis(deadline.saturating_sub(now)))
            .map_or(interval, |delay| delay.min(interval));
        match receiver.recv_timeout(delay) {
            Ok(ProcessReaperMessage::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
            Ok(ProcessReaperMessage::Wake(process)) => {
                let mut wake = BTreeSet::from([process]);
                if let Ok(ancestors) = process_ancestors(manager, process) {
                    wake.extend(ancestors);
                }
                for process in wake {
                    if let Err(error) =
                        schedule_finished_process(manager, process, &mut indexed, &mut deadlines)
                    {
                        eprintln!("Ousject Process cleanup scheduling failed: {error}");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn collect_expired_process_tree(
    manager: &Arc<InMemoryObjectManager>,
    root: ObjectId,
    cutoff: u64,
) -> Result<Option<Vec<ObjectId>>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut retired = BTreeSet::new();
    let mut discovery_order = Vec::new();
    let mut pending = vec![root];
    while let Some(object) = pending.pop() {
        if !retired.insert(object) {
            continue;
        }
        let view = manager.read(context, object)?;
        if view.header().type_id == CORE_PROCESS_TYPE {
            let state = decode_process_state(view.state())?;
            if !matches!(
                state.status,
                ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
            ) || state.ended_at_unix_ms.is_none_or(|ended| ended > cutoff)
                || view.links().contains_key("$effect")
            {
                return Ok(None);
            }
        }
        if view.header().type_id == CORE_EFFECT_TYPE {
            let effect = EffectRecord::decode(view.state())?;
            if matches!(
                effect.status,
                EffectStatus::Pending | EffectStatus::Running | EffectStatus::Unknown
            ) {
                return Ok(None);
            }
        }
        discovery_order.push(object);
        pending.extend(view.children().iter().copied());
    }

    Ok(Some(discovery_order))
}

fn retire_process_objects(
    manager: &Arc<InMemoryObjectManager>,
    retired: &BTreeSet<ObjectId>,
    discovery_order: &[ObjectId],
) -> Result<(), VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut transaction = manager.begin(context);
    for header in manager.list(context)? {
        if header.lifecycle == LifecycleState::Tombstoned {
            continue;
        }
        let view = manager.read(context, header.id)?;
        let mut remove_links = view
            .links()
            .iter()
            .filter(|(_, target)| retired.contains(target))
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        if retired.contains(&header.id) {
            remove_links.extend(view.links().keys().cloned());
        }
        if !remove_links.is_empty() {
            transaction.expect(header.id, header.version);
            for name in remove_links {
                transaction.remove_link(header.id, name);
            }
        }
        if header.type_id == PROCESS_TYPE && !retired.contains(&header.id) {
            let mut process = decode_process_state(view.state())?;
            if remove_retired_bindings(&mut process, retired) {
                transaction
                    .expect(header.id, header.version)
                    .update_state(header.id, encode_process_state(&process)?);
            }
        }
    }
    for object in discovery_order.iter().rev().copied() {
        let view = manager.read(context, object)?;
        transaction.expect(object, view.header().version);
        if let Some(parent) = view.header().parent_id {
            let parent_version = manager.inspect(context, parent)?.version;
            transaction.expect(parent, parent_version);
        }
        transaction.tombstone(object);
    }
    manager.commit(transaction)?;
    Ok(())
}

fn decode_optional_u64(reader: &mut StateReader<'_>) -> Result<Option<u64>, VmError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64()?)),
        _ => Err(invalid_state("invalid Process optional-time marker")),
    }
}
