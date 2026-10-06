use super::super::{
    BTreeSet, NET_RESOLVER_TYPE, ObjectId, ObjectProvider, ProviderError, ProviderOutcome,
    ToSocketAddrs, Value,
};
use super::adapter_error;

#[derive(Debug)]
pub(crate) struct HostResolverProvider;

impl ObjectProvider for HostResolverProvider {
    fn type_id(&self) -> oms_types::TypeId {
        NET_RESOLVER_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Resolver Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        let ("resolve", [Value::Text(hostname)]) = (capability, arguments) else {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
        };
        let mut addresses = (hostname.as_str(), 0)
            .to_socket_addrs()
            .map_err(adapter_error)?
            .map(|address| Value::Text(address.ip().to_string()))
            .collect::<Vec<_>>();
        addresses.sort_by_key(ToString::to_string);
        addresses.dedup();
        if addresses.is_empty() {
            return Err(ProviderError::Adapter(
                "hostname resolved to no addresses".to_owned(),
            ));
        }
        Ok(ProviderOutcome::result(Value::Array(addresses)))
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["resolve".to_owned()].into_iter().collect()
    }
}
