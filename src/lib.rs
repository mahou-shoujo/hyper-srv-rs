//! Fuck that for now.

#![deny(missing_docs)]

use futures::{try_ready, Async, Future, Poll};
use http::Error as HyperHttpError;
use hyper::{
    client::connect::{Connect, Destination},
    Error as HyperError, Uri,
};
use std::{error::Error, fmt, sync::Arc};
use trust_dns_resolver::{
    error::{ResolveError, ResolveErrorKind},
    lookup::SrvLookupFuture,
    AsyncResolver, BackgroundLookup,
};

/// A wrapper around Hyper's [`Connect`]or with ability to preresolve SRV DNS records
/// before supplying resulting `host:port` pair to the underlying connector.
///
/// [`Connect`]: ../hyper/client/connect/trait.Connect.html
pub struct ServiceConnector<C> {
    inner: Arc<C>,
    resolver: Option<AsyncResolver>,
}

impl<C> Connect for ServiceConnector<C>
where
    C: Connect,
{
    type Transport = C::Transport;

    type Error = ServiceError;

    type Future = ServiceConnecting<C>;

    fn connect(&self, dst: Destination) -> Self::Future {
        let fut = match &self.resolver {
            Some(resolver) if dst.port().is_some() => {
                ServiceConnectingKind::Preresolve {
                    connector: self.inner.clone(),
                    fut: resolver.lookup_srv(dst.host()),
                    dst: Some(dst),
                }
            },
            _ => {
                ServiceConnectingKind::Inner {
                    fut: self.inner.connect(dst),
                }
            },
        };
        ServiceConnecting(fut)
    }
}

impl<C> fmt::Debug for ServiceConnector<C>
where
    C: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ServiceConnector").field("inner", &self.inner).finish()
    }
}

impl<C> ServiceConnector<C> {
    /// Creates a new instance of [`ServiceConnector`] with provided connector and
    /// optional DNS resolver. If the resolver is set to None all connections will be
    /// handled directly by the underlying connector. This allows to toggle SRV resolving
    /// mechanism without changing a type of connector used
    /// in a client (as it must be named and can not even be made into a trait object).
    ///
    /// [`ServiceConnector`]: struct.ServiceConnector.html
    pub fn new(inner: C, resolver: Option<AsyncResolver>) -> Self {
        ServiceConnector {
            inner: Arc::new(inner),
            resolver,
        }
    }
}

#[derive(Debug)]
enum ServiceErrorKind {
    HyperHttp(HyperHttpError),
    Hyper(HyperError),
    Resolve(ResolveError),
    Inner(Box<dyn Error + Send + Sync>),
}

/// An error type used in [`ServiceConnector`].
///
/// [`ServiceConnector`]: struct.ServiceConnector.html
#[derive(Debug)]
pub struct ServiceError(ServiceErrorKind);

impl From<HyperHttpError> for ServiceError {
    fn from(error: HyperHttpError) -> Self {
        ServiceError(ServiceErrorKind::HyperHttp(error))
    }
}

impl From<HyperError> for ServiceError {
    fn from(error: HyperError) -> Self {
        ServiceError(ServiceErrorKind::Hyper(error))
    }
}

impl From<ResolveError> for ServiceError {
    fn from(error: ResolveError) -> Self {
        ServiceError(ServiceErrorKind::Resolve(error))
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.0 {
            ServiceErrorKind::HyperHttp(err) => fmt::Display::fmt(err, f),
            ServiceErrorKind::Hyper(err) => fmt::Display::fmt(err, f),
            ServiceErrorKind::Resolve(err) => fmt::Display::fmt(err, f),
            ServiceErrorKind::Inner(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.0 {
            ServiceErrorKind::HyperHttp(err) => Some(err),
            ServiceErrorKind::Hyper(err) => Some(err),
            ServiceErrorKind::Inner(err) => Some(err.as_ref()),
            ServiceErrorKind::Resolve(_) => None,
        }
    }
}

impl ServiceError {
    fn inner<E>(inner: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        ServiceError(ServiceErrorKind::Inner(inner.into()))
    }
}

#[allow(clippy::large_enum_variant)]
enum ServiceConnectingKind<C>
where
    C: Connect,
{
    Preresolve {
        connector: Arc<C>,
        fut: BackgroundLookup<SrvLookupFuture>,
        dst: Option<Destination>,
    },
    Inner {
        fut: C::Future,
    },
}

impl<C> fmt::Debug for ServiceConnectingKind<C>
where
    C: Connect,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ServiceConnectingKind").finish()
    }
}

/// This future represents a connection in progress returned by [`ServiceConnector`].
///
/// [`ServiceConnector`]: struct.ServiceConnector.html
#[derive(Debug)]
pub struct ServiceConnecting<C>(ServiceConnectingKind<C>)
where
    C: Connect;

impl<C> Future for ServiceConnecting<C>
where
    C: Connect,
{
    type Item = <C::Future as Future>::Item;
    type Error = ServiceError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match &mut self.0 {
            ServiceConnectingKind::Preresolve {
                connector,
                fut,
                dst,
            } => {
                let response = try_ready!(fut.poll().map(|res| res.map(Some)).or_else(|err| {
                    match err.kind() {
                        ResolveErrorKind::NoRecordsFound {
                            ..
                        } => Ok(Async::Ready(None)),
                        _ => Err(ServiceError(ServiceErrorKind::Resolve(err))),
                    }
                }));
                let dst = dst.take().expect("double ready on preresolve future");
                let dst = match response.as_ref().and_then(|response| response.iter().next()) {
                    Some(srv) => {
                        let authority = format!("{}:{}", srv.target(), srv.port());
                        let uri = Uri::builder()
                            .scheme(dst.scheme())
                            .authority(authority.as_str())
                            .path_and_query("/")
                            .build()?;
                        Destination::try_from_uri(uri)?
                    },
                    None => dst,
                };
                {
                    *self = ServiceConnecting(ServiceConnectingKind::Inner {
                        fut: connector.connect(dst),
                    });
                }
                self.poll()
            },
            ServiceConnectingKind::Inner {
                fut,
            } => fut.poll().map_err(ServiceError::inner),
        }
    }
}
