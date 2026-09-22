# The bind test can fail

`test_http_addr_from_env` asserts that a listener created with
`GOVEE_HTTP_ADDR=127.0.0.1` reports `127.0.0.1` as its local address. To show
that the assertion is load-bearing rather than vacuous, `bind_http_listener`
was temporarily changed to ignore its `addr` argument:

```rust
tokio::net::TcpListener::bind((IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
```

`cargo test --all -- test_http_addr_from_env` then failed:

```
thread 'service::http::test_http_addr_from_env' panicked at src/service/http.rs:364:5:
assertion `left == right` failed
  left: 0.0.0.0
 right: 127.0.0.1

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 362 filtered out
```

The change was reverted; it is not part of this branch.

## The empty-value guard can fail too

`http_addr_from_env` refuses an empty or whitespace-only `$GOVEE_HTTP_ADDR`
with a purpose-written message rather than falling back to `0.0.0.0`. To show
that assertion is load-bearing, the guard was disabled (`if false && …`). The
test then failed:

```
thread 'service::http::test_http_addr_from_env' panicked at src/service/http.rs:390:9:
empty: $GOVEE_HTTP_ADDR="" is not an IP address: invalid IP address syntax

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 362 filtered out
```

Note what that failure shows: without the guard the value still does not bind
to every interface, because an empty string is not a valid address either. The
guard buys a message that says what to do, not the refusal itself. That change
was reverted too.

`local-checks.log` records the gates on the committed tree, where every one of
these tests passes.
