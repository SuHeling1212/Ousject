#![allow(clippy::wildcard_imports)]

use super::*;

const AUDIT_MAX_DETAILS_BYTES: usize = 4 * 1024;

impl VirtualMachine {
    /// Stages one append-only security or lifecycle event with the transaction
    /// that performs the audited action.
    pub(super) fn stage_audit_event(
        &self,
        actor: SubjectId,
        action: &str,
        target: ObjectId,
        details: Value,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        if !matches!(
            action,
            "permission.grant"
                | "permission.revoke"
                | "module.install"
                | "module.enable"
                | "module.disable"
                | "module.upgrade"
                | "module.rollback"
                | "module.uninstall"
                | "package.install"
                | "package.upgrade"
                | "package.rollback"
                | "package.uninstall"
                | "process.start"
                | "process.resume"
                | "process.suspend"
                | "process.terminate"
                | "effect.intent"
                | "effect.running"
                | "effect.pending"
                | "effect.completed"
                | "effect.failed"
                | "effect.unknown"
                | "security.denial"
                | "system.restart"
                | "system.shutdown"
        ) {
            return Err(invalid_state("unknown Audit action"));
        }
        let details = redact_audit_details(details);
        if details.encode()?.len() > AUDIT_MAX_DETAILS_BYTES {
            return Err(VmError::TypeError("Audit details exceed 4 KiB"));
        }

        let system = AccessContext::new(SYSTEM_SUBJECT);
        let roots = self.manager.query(
            system,
            &ObjectQuery::new().with_type(oms_types::CORE_AUDIT_TYPE),
        )?;
        let (root_id, root_version) = match roots.as_slice() {
            [root] => {
                let root_view = self.manager.read(system, root.id)?;
                (root.id, Some(root_view.header().version))
            }
            [] => {
                let root_id = ObjectId::new();
                let mut root = CreateObject::new(
                    oms_types::CORE_AUDIT_TYPE,
                    Value::Record(BTreeMap::from([(
                        "name".to_owned(),
                        Value::Text("audit".to_owned()),
                    )]))
                    .encode()?,
                )
                .with_id(root_id);
                root.capabilities = [
                    Capability::Inspect,
                    Capability::ViewValue,
                    Capability::CreateChild,
                    Capability::Link,
                ]
                .into_iter()
                .collect();
                transaction.create(root);
                (root_id, None)
            }
            _ => return Err(invalid_state("Audit root is duplicated")),
        };
        let event = ObjectId::new();
        let value = Value::Record(BTreeMap::from([
            ("action".to_owned(), Value::Text(action.to_owned())),
            ("actor".to_owned(), Value::Text(actor.to_string())),
            ("target".to_owned(), Value::Text(target.to_string())),
            (
                "time_unix_ms".to_owned(),
                Value::Integer(
                    i64::try_from(unix_time_millis())
                        .map_err(|_| invalid_state("Audit time is out of range"))?,
                ),
            ),
            ("details".to_owned(), details),
        ]));
        let mut request = CreateObject::new(oms_types::CORE_AUDIT_EVENT_TYPE, value.encode()?)
            .with_id(event)
            .with_parent(root_id);
        request.capabilities = [Capability::Inspect, Capability::ViewValue]
            .into_iter()
            .collect();
        if let Some(version) = root_version {
            transaction.expect(root_id, version);
        }
        transaction
            .create(request)
            .set_link(root_id, format!("event:{event}"), event);
        Ok(event)
    }
}

fn redact_audit_details(value: Value) -> Value {
    match value {
        Value::Text(text) if text.starts_with("secret:") => Value::Text("<redacted>".to_owned()),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(redact_audit_details).collect())
        }
        Value::Map(fields) => Value::Map(redact_audit_fields(fields)),
        Value::Record(fields) => Value::Record(redact_audit_fields(fields)),
        value => value,
    }
}

fn redact_audit_fields(fields: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    fields
        .into_iter()
        .map(|(name, value)| {
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "password"
                    | "passphrase"
                    | "secret"
                    | "token"
                    | "credential"
                    | "arguments"
                    | "result"
            ) {
                (name, Value::Text("<redacted>".to_owned()))
            } else {
                (name, redact_audit_details(value))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::redact_audit_details;
    use oms_types::Value;
    use std::collections::BTreeMap;

    #[test]
    fn audit_details_redact_secrets_recursively() {
        let details = Value::Record(BTreeMap::from([
            (
                "nested".to_owned(),
                Value::Array(vec![Value::Text("secret:handle".to_owned())]),
            ),
            (
                "password".to_owned(),
                Value::Text("plain-text-password".to_owned()),
            ),
        ]));
        let redacted = redact_audit_details(details);
        assert_eq!(
            redacted,
            Value::Record(BTreeMap::from([
                (
                    "nested".to_owned(),
                    Value::Array(vec![Value::Text("<redacted>".to_owned())]),
                ),
                ("password".to_owned(), Value::Text("<redacted>".to_owned()),),
            ]))
        );
    }
}
