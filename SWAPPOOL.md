# SwapPool（交换池）

## What it is

`core.swap_pool` is a persistent Object that gives authorized Processes a named place to discover shared Objects. Multiple Processes may attach to the same pool and use the returned Object IDs. The pool shares Object identity and committed Object state; it does not share RAM addresses.

Praxis has no pointer, reference, memory address, shared page, `mmap`, or Rust reference for a pool member. A member remains an ordinary Object with its same ObjectId, Type, owner, permissions, capabilities, version, value, links, and lifetime.

## Membership representation

Pool membership is stored as `member:<name>` Links on the SwapPool Object. Creating a pool uses the normal `object.create` path. Create an ordinary Object, then call `pool.attach(name, object_id)` to add it. `pool.get(name)` returns its ObjectId as Text; `pool.list()` returns a name to ObjectId Record; `pool.contains(name)` checks the membership; and `pool.detach(name)` removes only the membership Link.

```praxis
pool = object.create("core.swap_pool", {})
shared = object.create("core.value", {count: 0})
pool.attach("state", shared.id)

found_id = pool.get("state")
found = object.find(found_id)
found.replace({count: 1})
```

Processes that need the same pool must receive its Object ID and have access to the pool. They can then discover the same member by name. Process-local variable bindings are not shared.

## Permissions

Pool access and member access are separate checks. `attach` requires `Link` on the pool and `Inspect` on the member. `detach` requires `Link` on the pool. `get` and `list` require `Inspect` on each returned member; `contains` only reports whether a name is present. Finding an Object ID does not grant `ViewValue`, `ReplaceValue`, `Invoke`, or other capabilities on that Object. The normal Object API checks those capabilities when a Process reads or modifies it.

The pool does not grant members' rights to its callers, and attaching does not change a member's owner or parent. The current default pool capabilities are `Inspect`, `ViewValue`, `Invoke`, `Link`, `ManagePolicy`, and `Retire`; OMS policy still checks the calling Subject and each member's capabilities.

## Transactions and conflicts

Membership updates are ordinary OMS Link updates. A membership change and the Process token advance that requested it commit in the same transaction. Member value changes use ordinary Object replacement/transactions, including expected Object versions and optimistic conflict detection. A transaction that modifies several members can include all of them in the normal OMS transaction; SwapPool has no separate transaction engine.

Changes become visible only after the OMS persistent write and atomic publish succeed. If persistence fails, both visible and recovered state remain at the previous commit. Two writers based on the same Object version cannot silently overwrite each other: the stale write conflicts and must reread/retry.

## Detach, retire, and recovery

`detach(name)` deletes the pool's membership Link. It does not retire the member, change its ObjectId, or remove its other links. Retiring a member remains an explicit Object lifecycle operation and can clean up its own links under OMS rules.

The pool, links, and member Objects are persisted by OMS. After restart, the pool returns its last committed membership and members return their last committed state. Uncommitted changes are not visible. The runtime enforces up to 4096 members per pool and 64 pools created per Subject.

## Limits

The current implementation accepts named membership in existing Objects. It does not allocate memory, provide pool-scoped implicit capabilities, create a member by name, or implement broadcast messaging. Ordinary `object.create` and capability rules remain in force.
