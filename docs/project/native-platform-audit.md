# Native Platform Repository Audit

Audit of the `main` working tree at HEAD `3e81b86` on 2026-10-08. The Native
heap/OMS work described below is present in the working tree and remains
uncommitted.

## Findings

### Crate boundaries and host dependencies

| Crate | Platform status | Host-dependent parts |
|---|---|---|
| `oms-types`, `oms-shard`, `tf-format`, `praxis-compiler` | Shared `no_std + alloc` core | Native ObjectId generation requires an explicit boot entropy prefix. These libraries build without `std`. |
| `oms-runtime` | Shared `no_std + alloc` in-memory core plus Hosted adapters | Native uses the same transaction validation/application/publication path over `BTreeMap` state and single-core borrow-checked access. Hosted retains `im`, locks, `FileSnapshotBackend`, process-id lease, Unix liveness, worker threads and wall-clock retention. |
| `ousject-provider` | Shared provider contracts and Terminal screen model | Uses standard collections and synchronization; it does not itself access physical devices. |
| `ousject-auth` | Domain logic is reusable, implementation is host-bound through dependencies | `rand_core/getrandom` provides entropy; uses `std` time and collections and depends on host-backed `oms-runtime`. |
| `ousject-vm` | Existing execution semantics remain Host-bound | `std` collections/locks/channels; an OS thread for ProcessReaper; `Instant` and `SystemTime`; `/dev/urandom`; `ureq`/network for Package Market; Host auth/provider dependencies. It has not yet been connected to Native OMS. |
| `ousject-cli` | Host adapter and developer tool | Host files, process environment, stdin/stdout, terminal ioctl and `stty`, TCP/DNS, file-backed block emulation, and filesystem package/source loading. |
| `oms-tools` | Host-only developer utility | Process arguments, stdin/stdout, and CLI entry point. |
| `ousject-platform` | Minimal `no_std` mechanism contract | No host calls or allocation. It defines boot memory metadata, monotonic clock, entropy, Terminal byte transport, block transport with flush, and machine shutdown/reboot. Hosted runtime does not yet use these interfaces. |
| `ousject-native-image` | UEFI x86_64 early kernel with tested in-memory OMS bootstrap | Standalone EFI image target; firmware Serial I/O before handoff, polling COM1 afterward, owned GDT/TSS/IDT/stack, 100 Hz PIT/8259 timer, 8 MiB bootstrap heap and actual shared OMS Object/Transaction smoke. It is not yet a VM kernel. |

`std::collections`, `String`, `Vec`, `fmt`, UTF-8 helpers, and error traits are
primarily allocation or language conveniences. Locks, atomics, channels,
threads, filesystem, process identity, sockets, terminal APIs, wall clock, and
`getrandom` are actual OS dependencies. Removing the `std::` prefix alone
would not make the current OMS or VM runnable without a host kernel.

### Storage and durability

OMS persists a complete encoded store plus transaction after-images through
`SnapshotBackend`. `FileSnapshotBackend` implements generation manifest,
checkpoint, WAL, checksums, synchronization, group commit, and a filesystem
lease. Commit ordering is candidate validation, durable backend acknowledgement,
then in-memory publication. No block-device backend or native image writer is
present. VM Process/Object/Package/User/Capability state is durable OMS state;
the machine resources used to execute it are not.

### VM, scheduling, clock, entropy, and providers

The VM persists token position, stack, variables, call frames, handlers, status,
wait reason, timer deadline, and worker lease in Process state. Scheduler queues,
Timer heap, provider caches, Effect outcome caches, thread handles, and elapsed
`Instant` are rebuilt or local to one boot. A background ProcessReaper uses an
OS thread; Scheduler waiting currently sleeps the calling host thread. Timer and
process-retention policy currently use Unix wall-clock milliseconds, while
elapsed time uses `Instant`.
Random APIs read `/dev/urandom`; authentication uses `getrandom` through
`rand_core`. Package Market performs host HTTP requests through `ureq`.

Provider Registry is created during VM boot and sealed at first runnable Process
execution. Host hardware adapters currently publish and bind Terminal,
keyboard, resolver, network, and block-storage Objects. They are adapters, not
native device transports. Root Terminal is selected by `parent_id == None`;
child Terminals are restored as a hierarchy. Terminal bytes, screen/parser
state, host tty handles, and renderer caches remain ephemeral. Display remains
a separate Object and currently has no physical provider.

### Boot and userspace path

The current executable path is host CLI → open in-memory or FileSnapshotBackend
OMS → discover host adapters → construct VM and publish/reuse kernel service
Objects → install/recover `system/*.px` → create or authenticate User → execute
Praxis `system/init.px` → login and Shell. This uses the existing Praxis compiler,
OTF/TF format, VM, Scheduler, and OMS. A new Native image has a firmware entry
point, memory-map handoff, and a QEMU-verified post-handoff COM1 transport.
Its bootstrap physical allocator selects a contiguous range from UEFI
Conventional descriptors, respects alignment and address bounds, advances past
reservations, and rejects overlap with non-usable descriptors even if a
malformed map also labels that address usable. It remains a bootstrap selector,
not a general frame allocator or reclaiming free list.
BootInfo includes the CPU-reported physical-address width when available.
The Native image owns bootstrap identity page tables for the first 4 GiB using
2 MiB pages, with MMIO-described ranges uncached. It has no dynamic page-table
manager or mapping above 4 GiB. Its 8 MiB monotonic bootstrap heap is reserved
from a selected Conventional region below 4 GiB, so the identity map covers it.
Heap free is a no-op; there is no reclamation. The image also owns its GDT, TSS,
early fatal IDT, ring-0 execution stack, and dedicated NMI and double-fault
stacks. QEMU smoke modes verify CR3 handoff, task-register loading, `#UD`, panic
reporting, and `#DF` delivery. The current identity map is permissive and is
only an early single-address-space mapping; it does not provide process
isolation. Exceptions remain fatal; there is no recoverable exception policy,
privilege transition, or runtime stack allocator. A 100 Hz PIT on the legacy
8259 route drives a boot-local 10 ms resolution `MonotonicClock`; QEMU verifies
timer IRQ delivery and forward time. APIC routing and Scheduler sleep/wakeup
integration remain unimplemented. Invariant TSC is an optional clock source
only when CPUID supplies its frequency; this QEMU profile does not, so the PIT
clock is used.
QEMU verifies real `alloc` use (`Box`, growing `Vec`, `String`, `BTreeMap`,
`Arc`) and Native execution of the shared `oms-runtime`: it creates and reads an
Object, commits an update, creates Parent/Link relationships, checks denied
Capability access, and verifies a conflicted multi-operation transaction does
not partially publish. Native OMS remains volatile; there is no Native
Terminal Provider, VirtIO transport, raw-block OMS backend, or Host-to-Native
Store transfer.

### Durable identity and machine-local state

Durable identity is ObjectId/TypeId/SubjectId and the encoded state/relationships
stored in OMS, including Process, Program, Package, User, Capability, Links,
logical timer deadline, and Effect records. Ephemeral machine-local state
includes host paths and file handles, process/thread IDs, locks, sockets, tty
handles, terminal input buffers/screens, scheduler queues, and physical device
handles. Host provider identity strings in Object state describe the current
adapter and must not be treated as stable hardware identity during native
rebinding.

## Terminal rename audit

The requested legacy-symbol search found no `CONSOLE_TYPE`,
`CORE_CONSOLE_TYPE`, `ConsoleProvider`, `ConsoleObjectProvider`,
`TerminalObjectProvider`, `with_console`, `publish_console`, or
`object.find("console")` references in the tracked source. Current public model
is `core.terminal`. The VM's root selection filters for `parent_id == None`;
the host adapter also reconstructs parent/child routing. Current uncommitted VM
work adds a constructor that leaves the `core.terminal` Provider slot open for a
platform-specific Provider and rejects binding a child Terminal as root.

## Minimum native primitives still missing

1. General physical-memory ownership and frame reclamation beyond the current
   conservative bootstrap selector and monotonic 8 MiB heap.
2. Dynamic virtual-memory management, fine-grained permissions, mappings above
   4 GiB, a recoverable exception policy, and APIC-capable timer routing
   integrated with Scheduler sleep and wakeup.
3. Native serial transport exists for COM1 diagnostics; it still needs a
   `core.terminal` Provider and interactive input/session behavior.
4. A Native-compatible VM core; the current VM still depends on Host locks,
   time, process reaper, auth/provider APIs, sockets and package services.
5. Native OMS already starts in-memory without host filesystem workers or
   `nix` process-liveness logic; persistence integration remains future work.
6. Native authentication entropy policy and removal of `/dev/urandom`/host
   `getrandom` assumptions from native builds.
7. Native `init`/login/Shell entry using the same Praxis/VM/user-space code.

`ousject-platform` captures low-level contracts; `ousject-native-image` now
consumes BootInfo, selects a usable frame, installs an owned GDT/TSS/IDT, moves
to its own ring-0 stack, and emits post-handoff diagnostics under QEMU. The
shared `oms-types`, `tf-format`, and `praxis-compiler` crates now build with
`no_std + alloc`; Native ID creation requires an explicit boot entropy prefix.
Native acquires that prefix before `ExitBootServices` from UEFI RNG, falling
back to feature-detected RDSEED/RDRAND with bounded success checks. QEMU's
`-cpu max` supplied the hardware source used in the smoke; a machine without
either source halts before OMS startup. The trust boundary is firmware or CPU
entropy quality; no timer, address, or fixed prefix is used.

`oms-runtime` builds without `std`; Native uses the same Object and transaction
rules as Hosted while Host persistence, process leases, worker threads and Unix
liveness stay behind the `std` feature. QEMU verifies actual Object and
transaction behavior. This does not make the VM Native-ready: `ousject-vm`
still depends on Host time, entropy, locks, auth and provider APIs, networking,
and thread-based lifecycle helpers. The next Native milestone is adapting the
existing VM around platform services and executing an existing OTF Process; a
second interpreter would not meet that goal.
