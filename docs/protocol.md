# File Peeker gRPC API v1

File Peeker uses protobuf over plaintext HTTP/2 on IPv4 loopback. Local
sessions connect directly. Remote sessions carry the same channel through an
OpenSSH SOCKS5 tunnel, so the remote hop is SSH-encrypted. The protobuf package
is `file_peeker.v1`. The server crate owns the `.proto` source and exposes the
generated shared contract through `file_peeker_server::protocol`; the client
depends on that library module rather than a separate protocol crate.

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
rpc ResolvePath(ResolvePathRequest) returns (ResolvePathResponse);
rpc List(ListRequest) returns (stream ListBatch);
rpc Read(ReadRequest) returns (stream ReadChunk);
```

`ResolvePath` expands `~` and environment variables in the server environment,
makes relative inputs absolute against the server working directory, and
lexically removes `.` and `..`. It does not access the target, require it to
exist, or follow symbolic links. Already resolved paths are returned unchanged.

`List` accepts absolute, relative, and shell-style paths. The server uses its
own environment to expand `~` and `$VARIABLES`; relative results are interpreted
against its working directory. The server's core then reads the server host's
local filesystem through native APIs; it does not ask the client to resolve or
read the path. It streams zero or more non-empty batches and completes with gRPC
`OK`. The server groups at most 1024 core entries per chunk and splits encoded
protobuf messages at 1 MiB. The core itself does not batch or sort entries.

Errors may terminate a stream after valid batches, so callers retain partial
results. Core service cancellation terminates active enumeration with gRPC
`Cancelled`; dropping the response stream cancels unfinished enumeration.

`Read` applies the same selected-host path expansion and resolution rules, then
opens and validates a regular file from byte zero. Directories and other
non-regular targets fail before the stream starts. It streams ordered, non-empty
`ReadChunk` messages containing at most 64 KiB each and completes with gRPC
`OK`; an empty file emits no chunks. There is no range, seek, resume, metadata,
or content length interface. Open and validation failures are returned before
the stream starts, while a later filesystem failure terminates the stream after
any bytes already sent. Core service cancellation terminates an active read with
gRPC `Cancelled`; dropping the response stream cancels the unfinished read.
Chunk boundaries are transport details and clients must not depend on an exact
chunk count or split position. The server enforces the 64 KiB wire limit by
splitting core items of any size; the core interface itself has no maximum chunk
size. A successful empty `ReadChunk` violates the protocol and is exposed by the
Rust client as terminal invalid data.

## Status mapping

| Filesystem result | gRPC status |
| --- | --- |
| Missing path | `NotFound` |
| Permission failure | `PermissionDenied` |
| Non-directory list target | `FailedPrecondition` |
| Non-file read target | `FailedPrecondition` |
| Invalid or non-UTF-8 path | `InvalidArgument` |
| Service or operation cancellation | `Cancelled` |
| Other filesystem failure | `Internal` |

The client maps these statuses back to the appropriate Rust backend and public
operation error surfaces. API major version 1 remains reported by
`version --format json`.
