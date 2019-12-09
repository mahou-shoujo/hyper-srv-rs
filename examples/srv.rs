use futures::future::lazy;
use hyper::{client::HttpConnector, Body, Client, StatusCode, Uri};
use hyper_srv::ServiceConnector;
use tokio::runtime::Runtime;
use trust_dns_resolver::AsyncResolver;

pub fn main() {
    let mut runtime = Runtime::new().unwrap();
    let response = runtime
        .block_on(lazy(|| {
            let (resolver, resolver_fut) = AsyncResolver::from_system_conf().unwrap();
            tokio::spawn(resolver_fut);
            let client =
                Client::builder().build::<_, Body>(ServiceConnector::new(HttpConnector::new(1), Some(resolver)));
            client.get(Uri::from_static("http://_http._tcp.mxtoolbox.com"))
        }))
        .unwrap();
    // Cloudfront returns 403 but at least we have resolved SRV uri correctly.
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
