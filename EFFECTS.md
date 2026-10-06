# Durable Effects

An Effect records an external Provider operation that cannot be included in the local OMS transaction. The Process persists the Effect intent and enters a wait state before the Provider is called. The Provider result, any Provider-created/updated Object state, Effect completion, and Process wakeup are then committed together.

Effect statuses are `pending`, `running`, `completed`, `failed`, and `unknown`. Recovery policy is either `manual` or `retry_idempotent`.

- `retry_idempotent` retries an interrupted operation with the same Effect ID, which the Provider can use as its idempotency key.
- `manual` changes an interrupted in-flight Effect to `unknown`; it is not automatically replayed. The local Subject can inspect and explicitly resolve or retry it.
- A completed Effect is not reissued when the Process resumes.

This does not promise exactly-once behavior from a remote service that does not honor idempotency. A crash after the external service accepts a request but before local completion commits may leave a manual Effect `unknown` or cause a configured idempotent Provider to retry.

Effect arguments and results recursively redact Text values beginning with `secret:` before persistence. Secret handles remain opaque. Audit records include the Effect identity and lifecycle action, not argument or result payloads.
