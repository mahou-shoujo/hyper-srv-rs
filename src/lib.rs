//! This crate provides a wrapper around Hyper's connector with ability to preresolve SRV DNS records
//! before supplying resulting `host:port` pair to the underlying connector.
//! The exact algorithm is as following:
//!
//! 1) Check if a connection destination could be (theoretically) a srv record (has no port, etc).
//! Use the underlying connector otherwise.
//! 2) Try to resolve the destination host and port using provided resolver (if set). In case no
//! srv records has been found use the underlying connector with the origin destination.
//! 3) Use the first record resolved to create a new destination (`A`/`AAAA`) and
//! finally pass it to the underlying connector.

#![deny(missing_docs)]

use futures::{
    future::BoxFuture,
    ready,
    task::{Context, Poll},
    Future,
};
use hyper::{client::connect::Connection, service::Service, Uri};
use std::{error::Error, fmt, pin::Pin};
use tokio::io::{AsyncRead, AsyncWrite};
use trust_dns_resolver::{
    error::{ResolveError, ResolveErrorKind},
    lookup::SrvLookup,
    TokioAsyncResolver,
};

/// A wrapper around Hyper's [`Connect`]or with ability to preresolve SRV DNS records
/// before supplying resulting `host:port` pair to the underlying connector.
///
/// [`Connect`]: ../hyper/client/connect/trait.Connect.html
#[derive(Debug, Clone)]
pub struct ServiceConnector<C> {
    resolver: Option<TokioAsyncResolver>,
    inner: C,
}

impl<C> Service<Uri> for ServiceConnector<C>
where
    C: Service<Uri> + Clone + Unpin,
    C::Response: AsyncRead + AsyncWrite + Connection + Unpin + Send + 'static,
    C::Error: Into<Box<dyn Error + Send + Sync>>,
    C::Future: Unpin + Send,
{
    type Response = C::Response;
    type Error = ServiceError;
    type Future = ServiceConnecting<C>;

    fn poll_ready(&mut self, ctx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(ctx).map_err(ServiceError::inner)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let fut = match (&self.resolver, uri.host(), uri.port()) {
            (Some(resolver), Some(_), None) => {
                ServiceConnectingKind::Preresolve {
                    inner: self.inner.clone(),
                    fut: {
                        let resolver = resolver.clone();
                        Box::pin(async move {
                            let host = uri.host().expect("host was right here, now it is gone");
                            let resolved = resolver.srv_lookup(host).await;
                            (resolved, uri)
                        })
                    },
                }
            },
            _ => {
                ServiceConnectingKind::Inner {
                    fut: self.inner.call(uri),
                }
            },
        };
        ServiceConnecting(fut)
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
    pub fn new(inner: C, resolver: Option<TokioAsyncResolver>) -> Self {
        ServiceConnector {
            resolver,
            inner,
        }
    }
}

#[derive(Debug)]
enum ServiceErrorKind {
    Resolve(ResolveError),
    Inner(Box<dyn Error + Send + Sync>),
}

/// An error type used in [`ServiceConnector`].
///
/// [`ServiceConnector`]: struct.ServiceConnector.html
#[derive(Debug)]
pub struct ServiceError(ServiceErrorKind);

impl From<ResolveError> for ServiceError {
    fn from(error: ResolveError) -> Self {
        ServiceError(ServiceErrorKind::Resolve(error))
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.0 {
            ServiceErrorKind::Resolve(err) => fmt::Display::fmt(err, f),
            ServiceErrorKind::Inner(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.0 {
            ServiceErrorKind::Resolve(_) => None,
            ServiceErrorKind::Inner(err) => Some(err.as_ref()),
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
    C: Service<Uri> + Unpin,
{
    Preresolve {
        inner: C,
        fut: BoxFuture<'static, (Result<SrvLookup, ResolveError>, Uri)>,
    },
    Inner {
        fut: C::Future,
    },
}

impl<C> fmt::Debug for ServiceConnectingKind<C>
where
    C: Service<Uri> + Unpin,
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
    C: Service<Uri> + Unpin;

impl<C> Future for ServiceConnecting<C>
where
    C: Service<Uri> + Unpin,
    C::Response: AsyncRead + AsyncWrite + Connection + Unpin + Send + 'static,
    C::Error: Into<Box<dyn Error + Send + Sync>>,
    C::Future: Unpin + Send,
{
    type Output = Result<C::Response, ServiceError>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        match &mut self.0 {
            ServiceConnectingKind::Preresolve {
                inner,
                fut,
            } => {
                let (res, uri) = ready!(Pin::new(fut).poll(ctx));
                let response = res.map(Some).or_else(|err| {
                    match err.kind() {
                        ResolveErrorKind::NoRecordsFound {
                            ..
                        } => Ok(None),
                        _unexpected => Err(ServiceError(ServiceErrorKind::Resolve(err))),
                    }
                })?;
                let uri = match response.as_ref().and_then(|response| response.iter().next()) {
                    Some(srv) => {
                        let authority = format!("{}:{}", srv.target(), srv.port());
                        let builder = Uri::builder().authority(authority.as_str());
                        let builder = match uri.scheme() {
                            Some(scheme) => builder.scheme(scheme.clone()),
                            None => builder,
                        };
                        let builder = match uri.path_and_query() {
                            Some(path_and_query) => builder.path_and_query(path_and_query.clone()),
                            None => builder,
                        };
                        builder.build().map_err(ServiceError::inner)?
                    },
                    None => uri,
                };
                {
                    *self = ServiceConnecting(ServiceConnectingKind::Inner {
                        fut: inner.call(uri),
                    });
                }
                self.poll(ctx)
            },
            ServiceConnectingKind::Inner {
                fut,
            } => Pin::new(fut).poll(ctx).map_err(ServiceError::inner),
        }
    }
}
