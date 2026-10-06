# Inter-Process Communication

Hosted Core currently provides two Object based IPC mechanisms: persistent Channels for messages and SwapPools for named membership in shared Objects. Neither exposes addresses or host handles.

## Channel

Create with `object.create("core.channel", [])`. `send(value)` appends one encoded Value and atomically wakes registered waiters. `receive()` removes and returns the oldest message, or returns `null` when empty. `wait()` records the current Process as waiting when the queue is empty; a later send clears the wait registration and makes that Process ready in the same transaction.

Messages and queue state are Object state. Each message is limited to 1 MiB; a Channel holds at most 1024 messages and 8 MiB encoded state. A restart preserves unconsumed messages. A receive commits the dequeue with the receiving Process position, so a committed receive is not replayed after restart.

## SwapPool

SwapPool supplies named discovery links to existing Objects. See [SWAPPOOL.md](SWAPPOOL.md) for its membership, capability, transaction, and recovery rules.

## Scope

There is no Topic, subscription, broadcast, raw shared memory, or implicit access to another Process's local variables. The target Object's own permission checks still apply after discovery.
