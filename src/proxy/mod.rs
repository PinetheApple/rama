//! Upstream proxy types and utilities.
//!
//! See the [`ProxyFilter`] for more information on how to select a proxy,
//! and the [`ProxyDB`] trait for how to implement a proxy database.
//!
//! If you wish to support proxy filters directly from the username,
//! you can use the [`UsernameConfig`] to extract the proxy filter
//! from the username and add yourself it to the [`Context`]'s [`Extensions`].
//!
//! The [`ProxyDB`] is used by Connection Pools to connect via a proxy,
//! in case a [`ProxyFilter`] is present in the [`Context`]'s [`Extensions`].
//!
//! [`Context`]: crate::service::Context
//! [`Extensions`]: crate::service::context::Extensions

pub mod username;
pub use username::{parse_username_config, UsernameConfig};

pub mod pp;

mod proxydb;
pub use proxydb::{
    MemoryProxyDB, MemoryProxyDBError, MemoryProxyDBErrorKind, Proxy, ProxyCredentials, ProxyDB,
    ProxyFilter, RequestContext,
};
