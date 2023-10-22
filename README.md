## Hyper SRV connector
![Build Status](https://github.com/github/docs/actions/workflows/rust.yml/badge.svg)

This crate provides a wrapper around Hyper's connector with ability to preresolve SRV DNS records
before supplying resulting `host:port` pair to the underlying connector.
The exact algorithm is as following:

1) Check if a connection destination could be (theoretically) a srv record (has no port, etc).
Use the underlying connector otherwise.
1) Try to resolve the destination host and port using provided resolver (if set). In case no
srv records has been found use the underlying connector with the origin destination.
1) Use the first record resolved to create a new destination (`A`/`AAAA`) and
finally pass it to the underlying connector.
