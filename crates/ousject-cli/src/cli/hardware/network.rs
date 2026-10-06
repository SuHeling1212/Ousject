use super::super::{
    BTreeMap, BTreeSet, Duration, Mutex, ObjectId, ObjectProvider, ProviderError, ProviderOutcome,
    Read, Shutdown, TcpListener, TcpStream, ToSocketAddrs, Value, Write,
};
use super::adapter_error;

#[derive(Debug)]
enum HostEndpoint {
    Stream(TcpStream),
    Listener(TcpListener),
}

#[derive(Debug, Default)]
pub(crate) struct HostNetworkProvider {
    endpoints: Mutex<BTreeMap<ObjectId, HostEndpoint>>,
    completed: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
}

impl ObjectProvider for HostNetworkProvider {
    fn type_id(&self) -> oms_types::TypeId {
        oms_types::NET_ENDPOINT_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        let (Value::Map(fields) | Value::Record(fields)) = initial else {
            return Err(ProviderError::InvalidArguments(
                "Network Endpoint requires a configuration Map",
            ));
        };
        if !match fields.get("transport") {
            None => true,
            Some(Value::Text(value)) => value == "tcp",
            Some(_) => false,
        } {
            return Err(ProviderError::InvalidArguments(
                "only the tcp transport is currently available",
            ));
        }
        Ok(network_state("tcp", "new", None))
    }

    #[allow(clippy::too_many_lines)]
    fn invoke(
        &self,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if let Some(outcome) = self
            .completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&effect)
            .cloned()
        {
            return Ok(outcome);
        }
        let outcome = match (capability, arguments) {
            ("connect", [Value::Text(host), Value::Integer(port)]) => {
                let address = socket_address(host, *port)?;
                let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
                    .map_err(adapter_error)?;
                configure_stream(&stream)?;
                self.endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(object, HostEndpoint::Stream(stream));
                ProviderOutcome::result(Value::Null).with_state(network_state(
                    "tcp",
                    "connected",
                    Some(address.to_string()),
                ))
            }
            ("listen", [Value::Text(host), Value::Integer(port)]) => {
                let address = socket_address(host, *port)?;
                let listener = TcpListener::bind(address).map_err(adapter_error)?;
                listener.set_nonblocking(true).map_err(adapter_error)?;
                let local = listener.local_addr().map_err(adapter_error)?.to_string();
                self.endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(object, HostEndpoint::Listener(listener));
                ProviderOutcome::result(Value::Text(local.clone())).with_state(network_state(
                    "tcp",
                    "listening",
                    Some(local),
                ))
            }
            ("accept", []) => {
                let (stream, peer) = {
                    let endpoints = self
                        .endpoints
                        .lock()
                        .map_err(|_| ProviderError::Unavailable)?;
                    let Some(HostEndpoint::Listener(listener)) = endpoints.get(&object) else {
                        return Err(ProviderError::Adapter(
                            "Endpoint is not listening".to_owned(),
                        ));
                    };
                    listener.accept().map_err(adapter_error)?
                };
                configure_stream(&stream)?;
                let child = ObjectId::new();
                self.endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(child, HostEndpoint::Stream(stream));
                let child_state = network_state("tcp", "connected", Some(peer.to_string()));
                let request = oms_runtime::CreateObject::new(
                    oms_types::NET_ENDPOINT_TYPE,
                    child_state.encode()?,
                )
                .with_id(child)
                .with_parent(object);
                ProviderOutcome::result(Value::Text(child.to_string())).with_created(request)
            }
            ("send", [data]) => {
                let bytes = match data {
                    Value::Bytes(bytes) => bytes.as_slice(),
                    Value::Text(text) => text.as_bytes(),
                    _ => {
                        return Err(ProviderError::InvalidArguments(
                            "send requires Text or Bytes",
                        ));
                    }
                };
                let mut endpoints = self
                    .endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?;
                let Some(HostEndpoint::Stream(stream)) = endpoints.get_mut(&object) else {
                    return Err(ProviderError::Adapter(
                        "Endpoint is not connected".to_owned(),
                    ));
                };
                stream.write_all(bytes).map_err(adapter_error)?;
                ProviderOutcome::result(Value::Integer(i64::try_from(bytes.len()).map_err(
                    |_| ProviderError::Adapter("sent byte count is too large".to_owned()),
                )?))
            }
            ("receive", [] | [Value::Null]) => receive_from(&self.endpoints, object, 65_536)?,
            ("receive", [Value::Integer(maximum)]) => {
                let maximum = usize::try_from(*maximum)
                    .ok()
                    .filter(|value| *value > 0 && *value <= 16 * 1024 * 1024)
                    .ok_or(ProviderError::InvalidArguments(
                        "receive size must be between 1 and 16777216",
                    ))?;
                receive_from(&self.endpoints, object, maximum)?
            }
            ("close", []) => {
                if let Some(HostEndpoint::Stream(stream)) = self
                    .endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .remove(&object)
                {
                    match stream.shutdown(Shutdown::Both) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
                        Err(error) => return Err(adapter_error(error)),
                    }
                }
                ProviderOutcome::result(Value::Null)
                    .with_state(network_state("tcp", "closed", None))
            }
            _ => {
                return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
            }
        };
        self.completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(effect, outcome.clone());
        Ok(outcome)
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["connect", "listen", "accept", "send", "receive", "close"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

fn socket_address(host: &str, port: i64) -> Result<std::net::SocketAddr, ProviderError> {
    let port = u16::try_from(port)
        .map_err(|_| ProviderError::InvalidArguments("port must be between 0 and 65535"))?;
    (host, port)
        .to_socket_addrs()
        .map_err(adapter_error)?
        .next()
        .ok_or_else(|| ProviderError::Adapter("address did not resolve".to_owned()))
}

fn configure_stream(stream: &TcpStream) -> Result<(), ProviderError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(adapter_error)
}

fn receive_from(
    endpoints: &Mutex<BTreeMap<ObjectId, HostEndpoint>>,
    object: ObjectId,
    maximum: usize,
) -> Result<ProviderOutcome, ProviderError> {
    let mut endpoints = endpoints.lock().map_err(|_| ProviderError::Unavailable)?;
    let Some(HostEndpoint::Stream(stream)) = endpoints.get_mut(&object) else {
        return Err(ProviderError::Adapter(
            "Endpoint is not connected".to_owned(),
        ));
    };
    let mut bytes = vec![0_u8; maximum];
    let count = stream.read(&mut bytes).map_err(adapter_error)?;
    bytes.truncate(count);
    Ok(ProviderOutcome::result(Value::Bytes(bytes)))
}

fn network_state(transport: &str, status: &str, peer: Option<String>) -> Value {
    Value::Record(BTreeMap::from([
        ("transport".to_owned(), Value::Text(transport.to_owned())),
        ("status".to_owned(), Value::Text(status.to_owned())),
        ("peer".to_owned(), peer.map_or(Value::Null, Value::Text)),
    ]))
}
