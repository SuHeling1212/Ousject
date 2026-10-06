# Persistent Process Scheduler

This document describes the Hosted Core scheduler in `ousject-vm`. It is an Ousject mechanism; Praxis does not receive a scheduler Object.

## Process state

Each `core.process` stores one of `Ready`, `Running`, `Waiting`, `Suspended`, `Halted`, `Terminated`, or `Failed`. `Waiting` includes a `WaitReason`: `Timer`, `Ipc`, `Effect`, `Input`, or `Process`. The reason and its target/deadline are persisted with the token position, stack, call frames, variables, and error/result.

## Worker leases and execution slices

The cooperative scheduler keeps ready Process IDs in a round-robin queue. A Worker claims a persisted lease with an owner ID, generation, and 30-second deadline. It renews the lease while running. Process state and all Object changes from each execution slice commit together; a failed transaction publishes neither. Slices stop after 4096 instructions or 20 ms, whichever comes first.

Every lease claim increments the generation. A commit checks the current owner and generation, so an expired Worker cannot publish after a replacement Worker has claimed and advanced the Process. Store recovery clears stale leases, advances their generations, and returns interrupted `Running` Processes to `Ready`.

## Waiting and recovery

Waiting Processes do not occupy an execution Worker. Channel sends and Timer expiration commit the wakeup with the message or Timer state change. An idle scheduler sleeps until the next persisted Timer deadline instead of repeatedly running a sleeping Process. `CooperativeScheduler::recover` rebuilds the runnable queue from persistent Process state.

An execution slice already committed before shutdown is not replayed: its token position and Object writes share the same transaction. A slice whose commit failed remains at its old position and can be retried.

## Hosted boundary

The current scheduler is cooperative and hosted by the Rust runtime. A Worker is an execution lease and call path, not an Ousject Process Object or a Praxis-visible host thread. This implementation does not claim preemptive scheduling or multi-core kernel support.
