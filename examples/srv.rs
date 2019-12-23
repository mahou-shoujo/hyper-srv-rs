use hyper::{client::HttpConnector, Body, Client, StatusCode, Uri};
use hyper_srv::ServiceConnector;
use tokio::runtime::Runtime;
use trust_dns_resolver::TokioAsyncResolver;

pub fn main() {
    let mut rt = Runtime::new().unwrap();
    let handle = rt.handle().clone();
    rt.block_on(async move {
        let resolver = TokioAsyncResolver::from_system_conf(handle).await.unwrap();
        let client = Client::builder().build::<_, Body>(ServiceConnector::new(HttpConnector::new(), Some(resolver)));
        let response = client.get(Uri::from_static("http://_http._tcp.mxtoolbox.com")).await.unwrap();
        // Cloudfront returns 403 but at least we have resolved SRV uri correctly.
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    });
}
