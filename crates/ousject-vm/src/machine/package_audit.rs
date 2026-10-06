#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    pub(super) fn stage_package_audit(
        &self,
        owner: SubjectId,
        registry: ObjectId,
        action: &str,
        target: ObjectId,
        details: Value,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        if !matches!(
            action,
            "build"
                | "import"
                | "install"
                | "run"
                | "export"
                | "upgrade"
                | "rollback"
                | "uninstall"
                | "restore"
        ) {
            return Err(invalid_state("unknown Package audit action"));
        }
        let encoded_details = details.encode()?;
        if encoded_details.len() > 64 * 1024 {
            return Err(VmError::TypeError(
                "Package audit details exceed the 64 KiB limit",
            ));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let registry_view = self.manager.read(system, registry)?;
        if registry_view.header().type_id != CORE_PACKAGE_REGISTRY_TYPE {
            return Err(invalid_state(
                "Package audit parent is not the Package Registry",
            ));
        }
        let audit = ObjectId::new();
        let value = Value::Record(BTreeMap::from([
            ("action".to_owned(), Value::Text(action.to_owned())),
            ("owner".to_owned(), Value::Text(owner.to_string())),
            ("target".to_owned(), Value::Text(target.to_string())),
            (
                "time_unix_ms".to_owned(),
                Value::Integer(
                    i64::try_from(unix_time_millis())
                        .map_err(|_| invalid_state("Package audit time is out of range"))?,
                ),
            ),
            ("details".to_owned(), details),
        ]));
        let mut request = CreateObject::new(CORE_PACKAGE_AUDIT_TYPE, value.encode()?)
            .with_id(audit)
            .with_parent(registry)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue);
        request.capabilities = [Capability::Inspect, Capability::ViewValue]
            .into_iter()
            .collect();
        transaction
            .expect(registry, registry_view.header().version)
            .create(request)
            .set_link(registry, format!("audit:{owner}:{audit}"), audit);
        Ok(audit)
    }
}
