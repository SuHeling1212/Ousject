#![allow(clippy::wildcard_imports)]

use super::*;

const SWAP_POOL_MEMBER_PREFIX: &str = "member:";
const SWAP_POOL_MAX_MEMBERS: usize = 4_096;
const SWAP_POOL_MAX_PER_SUBJECT: usize = 64;

impl VirtualMachine {
    pub(super) fn prepare_swap_pool_object(
        &self,
        initial: &Value,
        parent: ObjectId,
    ) -> Result<CreateObject, VmError> {
        let mut fields = match initial {
            Value::Map(fields) | Value::Record(fields) => fields.clone(),
            _ => {
                return Err(VmError::TypeError(
                    "SwapPool initial state must be a Record",
                ));
            }
        };
        if fields.contains_key("owner_subject") {
            return Err(VmError::TypeError(
                "owner_subject is reserved for the kernel",
            ));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let mut owned = 0;
        for header in self.manager.query(
            system,
            &ObjectQuery::new().with_type(oms_types::CORE_SWAP_POOL_TYPE),
        )? {
            if matches!(
                self.manager.value(system, header.id)?,
                Value::Record(ref value)
                    if value.get("owner_subject")
                        == Some(&Value::Text(self.context.subject.to_string()))
            ) {
                owned += 1;
            }
        }
        if owned >= SWAP_POOL_MAX_PER_SUBJECT {
            return Err(VmError::TypeError("Subject SwapPool limit exceeded"));
        }
        fields.insert(
            "owner_subject".to_owned(),
            Value::Text(self.context.subject.to_string()),
        );
        let mut request = self.manager.prepare_create(
            CreateSpec::new("core.swap_pool", Value::Record(fields)).with_parent(parent),
        )?;
        request.capabilities = [
            Capability::Inspect,
            Capability::ViewValue,
            Capability::Invoke,
            Capability::Link,
            Capability::ManagePolicy,
            Capability::Retire,
        ]
        .into_iter()
        .collect();
        Ok(request)
    }

    pub(super) fn invoke_swap_pool(
        &self,
        pool: ObjectId,
        capability: &str,
        arguments: &[Value],
        transaction: &mut Transaction,
    ) -> Result<(Value, Option<String>), VmError> {
        match (capability, arguments) {
            ("attach", [Value::Text(name), Value::Text(member_id)]) => {
                validate_namespace_name(name)?;
                let member = object_id(&Value::Text(member_id.clone()))?;
                self.manager
                    .require_capability(self.context, pool, Capability::Link)?;
                self.manager
                    .require_capability(self.context, member, Capability::Inspect)?;
                let view = self.manager.read(self.context, pool)?;
                let key = format!("{SWAP_POOL_MEMBER_PREFIX}{name}");
                if view.links().contains_key(&key) {
                    return Err(VmError::TypeError("SwapPool member name already exists"));
                }
                let member_count = view
                    .links()
                    .keys()
                    .filter(|key| key.starts_with(SWAP_POOL_MEMBER_PREFIX))
                    .count();
                if member_count >= SWAP_POOL_MAX_MEMBERS {
                    return Err(VmError::TypeError("SwapPool member limit exceeded"));
                }
                transaction
                    .expect(pool, view.header().version)
                    .set_link(pool, key, member);
                Ok((Value::Null, None))
            }
            ("detach", [Value::Text(name)]) => {
                validate_namespace_name(name)?;
                self.manager
                    .require_capability(self.context, pool, Capability::Link)?;
                let view = self.manager.read(self.context, pool)?;
                let key = format!("{SWAP_POOL_MEMBER_PREFIX}{name}");
                if !view.links().contains_key(&key) {
                    return Err(VmError::MissingKey(name.clone()));
                }
                transaction
                    .expect(pool, view.header().version)
                    .remove_link(pool, key);
                Ok((Value::Null, None))
            }
            ("get", [Value::Text(name)]) => {
                validate_namespace_name(name)?;
                let view = self.manager.read(self.context, pool)?;
                let member = view
                    .links()
                    .get(&format!("{SWAP_POOL_MEMBER_PREFIX}{name}"))
                    .copied()
                    .ok_or_else(|| VmError::MissingKey(name.clone()))?;
                self.manager
                    .require_capability(self.context, member, Capability::Inspect)?;
                Ok((Value::Text(member.to_string()), None))
            }
            ("contains", [Value::Text(name)]) => {
                validate_namespace_name(name)?;
                let view = self.manager.read(self.context, pool)?;
                Ok((
                    Value::Bool(
                        view.links()
                            .contains_key(&format!("{SWAP_POOL_MEMBER_PREFIX}{name}")),
                    ),
                    None,
                ))
            }
            ("list", []) => {
                let view = self.manager.read(self.context, pool)?;
                let mut members = BTreeMap::new();
                for (key, member) in view.links() {
                    let Some(name) = key.strip_prefix(SWAP_POOL_MEMBER_PREFIX) else {
                        continue;
                    };
                    self.manager
                        .require_capability(self.context, *member, Capability::Inspect)?;
                    members.insert(name.to_owned(), Value::Text(member.to_string()));
                }
                Ok((Value::Record(members), None))
            }
            _ => Err(VmError::TypeError(
                "SwapPool expects attach(name, object_id), detach(name), get(name), contains(name), or list()",
            )),
        }
    }
}
