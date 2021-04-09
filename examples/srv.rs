use hyper::{client::HttpConnector, Body, Client, StatusCode, Uri};
use hyper_srv::ServiceConnector;
use trust_dns_resolver::AsyncResolver;

#[tokio::main]
pub async fn main() {
    let resolver = AsyncResolver::tokio_from_system_conf().unwrap();
    let client = Client::builder().build::<_, Body>(ServiceConnector::new(HttpConnector::new(), Some(resolver)));
    let response = client.get(Uri::from_static("http://_http._tcp.mxtoolbox.com")).await.unwrap();
    // Cloudfront returns 403 but at least we have resolved SRV uri correctly.
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
