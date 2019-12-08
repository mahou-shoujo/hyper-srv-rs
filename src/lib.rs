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

pub struct HttpServiceConnector<C> {
    inner: Arc<C>,
    resolver: AsyncResolver,
}

impl<C> Connect for HttpServiceConnector<C>
where
    C: Connect,
{
    type Transport = C::Transport;

    type Error = HttpServiceError;

    type Future = HttpServiceConnecting<C>;

    fn connect(&self, dst: Destination) -> Self::Future {
        match dst.port() {
            Some(_) => {
                HttpServiceConnecting::Inner {
                    fut: self.inner.connect(dst),
                }
            },
            None => {
                HttpServiceConnecting::Preresolve {
                    connector: self.inner.clone(),
                    fut: self.resolver.lookup_srv(dst.host()),
                    dst: Some(dst),
                }
            },
        }
    }
}

impl<C> fmt::Debug for HttpServiceConnector<C>
where
    C: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("HttpServiceConnector").field("inner", &self.inner).finish()
    }
}

impl<C> HttpServiceConnector<C> {
    pub fn new(inner: C, resolver: AsyncResolver) -> Self {
        HttpServiceConnector {
            inner: Arc::new(inner),
            resolver,
        }
    }
}

#[derive(Debug)]
pub enum HttpServiceError {
    HyperHttp(HyperHttpError),
    Hyper(HyperError),
    TrustResolver(ResolveError),
    Inner(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for HttpServiceError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HttpServiceError::HyperHttp(err) => fmt::Display::fmt(err, f),
            HttpServiceError::Hyper(err) => fmt::Display::fmt(err, f),
            HttpServiceError::TrustResolver(err) => fmt::Display::fmt(err, f),
            HttpServiceError::Inner(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl Error for HttpServiceError {}

impl HttpServiceError {
    pub fn inner<E>(inner: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        HttpServiceError::Inner(inner.into())
    }
}

pub enum HttpServiceConnecting<C>
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

impl<C> Future for HttpServiceConnecting<C>
where
    C: Connect,
{
    type Item = <C::Future as Future>::Item;
    type Error = HttpServiceError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self {
            HttpServiceConnecting::Preresolve {
                connector,
                fut,
                dst,
            } => {
                let response = try_ready!(fut.poll().map(|res| res.map(|response| Some(response))).or_else(|err| {
                    match err.kind() {
                        ResolveErrorKind::NoRecordsFound {
                            ..
                        } => Ok(Async::Ready(None)),
                        _ => {
                            return Err(HttpServiceError::TrustResolver(err));
                        },
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
                            .build()
                            .map_err(HttpServiceError::HyperHttp)?;
                        Destination::try_from_uri(uri).map_err(HttpServiceError::Hyper)?
                    },
                    None => dst,
                };
                {
                    *self = HttpServiceConnecting::Inner {
                        fut: connector.connect(dst),
                    };
                }
                self.poll()
            },
            HttpServiceConnecting::Inner {
                fut,
            } => fut.poll().map_err(HttpServiceError::inner),
        }
    }
}

#[test]
fn test() {
    use futures::future::lazy;
    use hyper::{client::HttpConnector, Body, Client, StatusCode};
    use tokio::runtime::Runtime;
    let mut runtime = Runtime::new().unwrap();
    let response = runtime
        .block_on(lazy(|| {
            let (resolver, resolver_fut) = AsyncResolver::from_system_conf().unwrap();
            tokio::spawn(resolver_fut);
            let client = Client::builder().build::<_, Body>(HttpServiceConnector::new(HttpConnector::new(1), resolver));
            client.get(Uri::from_static("http://_http._tcp.mxtoolbox.com"))
        }))
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN); // CloudFront returns 403 but at least it works.
}
