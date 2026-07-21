# File Peeker gRPC API v1

File Peeker uses protobuf over plaintext HTTP/2 on IPv4 loopback. Local
sessions connect directly. Remote sessions carry the same channel through an
OpenSSH SOCKS5 tunnel, so the remote hop is SSH-encrypted. The protobuf package
is `file_peeker.v1`.

## Startup and authentication

`file-peeker-server serve` binds `127.0.0.1:0`, generates a memory-only 256-bit
token, and reports the selected endpoint before starting the gRPC server:

```text
FILE_PEEKER_SERVER_STARTUP={"port":43827,"token":"<64 lowercase hex characters>"}
```

The launcher keeps server stdin open as its lifetime lease. EOF or a termination
signal cancels active operations and triggers tonic graceful shutdown. The token
is never placed in arguments, environment, files, diagnostics, or errors.

Every gRPC request, including health checks, carries a sensitive metadata value:

```text
authorization: Bearer <session token>
```

Missing or invalid credentials return generic `Unauthenticated`. The server
binds only loopback and does not use TLS; SSH supplies encryption for remote
sessions.

## Services

The standard `grpc.health.v1.Health` service verifies startup readiness. The
client opens one persistent channel per Session, enables HTTP/2 keepalive, and
lets tonic reconnect it after transient transport loss. An interrupted RPC is
not replayed or resumed; later RPCs may use the reconnected channel.

`file_peeker.v1.FilePeeker` exposes:

```proto
rpc CurrentRoot(CurrentRootRequest) returns (CurrentRootResponse);
rpc List(ListRequest) returns (stream ListBatch);
```

`CurrentRoot` returns the server process's absolute UTF-8 working directory.

`List` accepts an absolute path, `~`, or `~/...`. It streams zero or more
non-empty batches and completes with gRPC `OK`. The server targets 1 MiB per
protobuf batch and flushes at 1024 entries or 25 ms after the first buffered
entry. It does not sort.

Errors may terminate a stream after valid batches, so callers retain partial
results. Dropping the response stream cancels unfinished enumeration.

## Status mapping

| Filesystem result | gRPC status |
| --- | --- |
| Missing path | `NotFound` |
| Permission failure | `PermissionDenied` |
| Non-directory | `FailedPrecondition` |
| Invalid or non-UTF-8 path | `InvalidArgument` |
| Other filesystem failure | `Internal` |

The client maps these statuses back to its existing Rust and UniFFI error
surfaces. API major version 1 remains reported by `version --format json`.
