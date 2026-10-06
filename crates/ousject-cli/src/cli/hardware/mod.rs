use super::ProviderError;

mod block_storage;
mod display;
mod keyboard;
mod network;
mod resolver;

pub(crate) use block_storage::HostBlockStorageProvider;
pub(crate) use display::{CachedProvider, HostDisplayProvider};
pub(crate) use keyboard::HostKeyboardProvider;
pub(crate) use network::HostNetworkProvider;
pub(crate) use resolver::HostResolverProvider;

#[allow(clippy::needless_pass_by_value)]
pub(super) fn adapter_error(error: std::io::Error) -> ProviderError {
    ProviderError::Adapter(error.to_string())
}
