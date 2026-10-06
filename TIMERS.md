# Persistent Timers

`core.timer` is a persistent Object with `armed`, `fired`, or `cancelled` state and an optional Unix-millisecond deadline. Its capabilities are `arm(milliseconds)`, `wait()`, `cancel()`, and `status()`.

`time.sleep(milliseconds)` creates an internal Timer and atomically stores its ID/deadline in the Process wait state. The Process becomes `Waiting` with `WaitReason::Timer`; it does not hold an execution Worker while asleep. Explicit Timer `wait()` links the Process to the Timer. Cancel and firing clear wait links and make matching waiters ready in the same transaction.

At startup, recovery scans armed Timers and fires expired ones. A failed fire transaction leaves the Timer armed and waiters asleep, so the next recovery attempt can retry it. Internal sleep Timers are tombstoned atomically with the wakeup; ordinary Timer Objects remain available in their fired state.

The Hosted Core uses the system wall clock for persisted deadlines. Sleep scheduling waits until the next deadline without busy-looping. Clock accuracy across host suspend or clock adjustment follows the host clock and is not a hardware monotonic-clock guarantee.
