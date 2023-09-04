//! DNS resolution via the [trust_dns_resolver](https://github.com/bluejekyll/trust-dns) crate

use hyper::client::connect::dns::Name;
use once_cell::sync::Lazy;
use tokio::sync::Mutex;
pub use trust_dns_resolver::config::{ResolverConfig, ResolverOpts};
use trust_dns_resolver::{
    lookup_ip::LookupIpIntoIter, system_conf, AsyncResolver, TokioConnection,
    TokioConnectionProvider, TokioHandle,
};

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use super::{Addrs, Resolve, Resolving};

use crate::error::BoxError;

type SharedResolver = Arc<AsyncResolver<TokioConnection, TokioConnectionProvider>>;

lazy_static! {
    static ref SYSTEM_CONF: Mutex<Lazy<io::Result<(ResolverConfig, ResolverOpts)>>> = {
        let data = Lazy::new(|| system_conf::read_system_conf().map_err(io::Error::from));
        Mutex::new(data)
    };
}

pub fn reinitialize_system_conf() {
    let mut conf = SYSTEM_CONF.lock().unwrap();
    *conf = Lazy::new(|| system_conf::read_system_conf().map_err(io::Error::from));
}

fn get_system_conf() -> io::Result<(ResolverConfig, ResolverOpts)> {
    let mut conf = SYSTEM_CONF.lock().unwrap();
    if conf.is_none() {
        *conf = Some(initialize_system_conf());
    }
    conf.clone().unwrap()
}

/// Wrapper around an `AsyncResolver`, which implements the `Resolve` trait.
#[derive(Debug, Clone)]
pub(crate) struct TrustDnsResolver {
    state: Arc<Mutex<State>>,
}

struct SocketAddrs {
    iter: LookupIpIntoIter,
}

#[derive(Debug)]
enum State {
    Init,
    Ready(SharedResolver),
}

impl TrustDnsResolver {
    /// Create a new resolver with the default configuration,
    /// which reads from `/etc/resolve.conf`.
    pub fn new() -> io::Result<Self> {
        get_system_conf().as_ref().map_err(|e| {
            io::Error::new(e.kind(), format!("error reading DNS system conf: {}", e))
        })?;

        // At this stage, we might not have been called in the context of a
        // Tokio Runtime, so we must delay the actual construction of the
        // resolver.
        Ok(TrustDnsResolver {
            state: Arc::new(Mutex::new(State::Init)),
        })
    }
}

impl Resolve for TrustDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.clone();
        Box::pin(async move {
            let mut lock = resolver.state.lock().await;

            let resolver = match &*lock {
                State::Init => {
                    let resolver = new_resolver().await?;
                    *lock = State::Ready(resolver.clone());
                    resolver
                }
                State::Ready(resolver) => resolver.clone(),
            };

            // Don't keep lock once the resolver is constructed, otherwise
            // only one lookup could be done at a time.
            drop(lock);

            let lookup = resolver.lookup_ip(name.as_str()).await?;
            let addrs: Addrs = Box::new(SocketAddrs {
                iter: lookup.into_iter(),
            });
            Ok(addrs)
        })
    }
}

impl Iterator for SocketAddrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|ip_addr| SocketAddr::new(ip_addr, 0))
    }
}

async fn new_resolver() -> Result<SharedResolver, BoxError> {
    let (config, opts) = get_system_conf()
        .as_ref()
        .expect("can't construct TrustDnsResolver if SYSTEM_CONF is error")
        .clone();
    new_resolver_with_config(config, opts)
}

fn new_resolver_with_config(
    config: ResolverConfig,
    opts: ResolverOpts,
) -> Result<SharedResolver, BoxError> {
    let resolver = AsyncResolver::new(config, opts, TokioHandle)?;
    Ok(Arc::new(resolver))
}
