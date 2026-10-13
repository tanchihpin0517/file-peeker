# File Peeker gRPC API v1

The File Peeker remote client/server boundary uses protobuf over plaintext
HTTP/2 on the server host's IPv4 loopback. Remote connections reach that
endpoint through an OpenSSH SOCKS5 tunnel, so the remote hop is SSH-encrypted.
Local connections speak the same protocol to a server embedded in the client
process, reaching its loopback port directly instead of through a tunnel. The
protobuf package is `file_peeker.v1`. The `.proto` source sits at `proto/` in the
repository root and `build.rs` generates both halves into `crate::protocol`,
which the client adapters and the server ops share directly.

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
authorization: Bearer <connection token>
```

Missing or invalid credentials return generic `Unauthenticated`. The server
binds only loopback and does not use TLS; SSH supplies encryption for remote
connections.

## Services

The standard `grpc.health.v1.Health` service verifies startup readiness. The
client opens one authenticated channel per remote connection. Reconnection,
interrupted-operation and replay behavior are owned by
[Connection Lifecycle](connection-lifecycle.md).

`file_peeker.v1.FilePeeker` exposes:

```proto
rpc ResolvePath(ResolvePathRequest) returns (ResolvePathResponse);
rpc List(ListRequest) returns (stream ListBatch);
rpc Walk(WalkRequest) returns (stream WalkBatch);
rpc Read(ReadRequest) returns (stream ReadChunk);
rpc Write(stream WriteFrame) returns (WriteResponse);
```

`ResolvePath` expands `~` and environment variables in the server environment,
makes relative inputs absolute against the server working directory, and
lexically removes `.` and `..`. It does not access the target, require it to
exist, or follow symbolic links. Already resolved paths are returned unchanged.

### Selected-host names and paths

Every path and name in this protocol crosses as an **escaped** form, because a Unix
filename is any byte sequence but `/` and NUL while a protobuf `string` must be UTF-8.
The escape is invertible, so a name a listing reports still addresses the file it
names:

| Input | Escaped |
| --- | --- |
| a valid UTF-8 run | verbatim |
| the literal `%` | `%%` |
| a byte that is not valid UTF-8 | `%XX`, uppercase hexadecimal |

The marker is escaped too, and must be: a file really can be named `caf%80.txt`, and
escaping only the invalid bytes would collide it with the name holding byte `0x80`.
Every ordinary filename is therefore its own escaped form.

Decoding accepts `%%` and `%XX` for `XX` from `80` upward, and **rejects everything
else** — a lowercase digit, a truncated sequence, or a value below `0x80`. That last
rule is what stops a peer from smuggling a `/` or a NUL into what is meant to be a
single name; since every byte below `0x80` is valid UTF-8 alone, the escape never
emits one. Rejecting only uppercase keeps one file to one escaped spelling, so a path
stays a single map key and a single comparison. A malformed escape is
`InvalidArgument`.

The escape never emits `/`, so an escaped path has exactly the components the real
path has: joining, splitting and prefix tests work on it without decoding.

`List` accepts absolute, relative, and shell-style paths. The server uses its
own environment to expand `~` and `$VARIABLES`; relative results are interpreted
against its working directory. The server's core then reads the server host's
local filesystem through native APIs; it does not ask the client to resolve or
read the path. It streams zero or more non-empty batches and completes with gRPC
`OK`. The server groups at most 1024 core entries per chunk and splits encoded
protobuf messages at 1 MiB. The core itself does not batch or sort entries.

Errors may terminate a stream after valid batches, so callers retain partial
results. Only two conditions are terminal: iterating the directory failing, and
cancellation. Anything wrong with one entry is carried by that entry — an entry whose
metadata could not be read arrives with `metadata` absent and the reason in `defect`
— so a directory that is readable but not searchable reports every entry it has
rather than failing as a whole. Core service cancellation terminates active
enumeration with gRPC `Cancelled`; dropping the response stream cancels unfinished
enumeration.

Each `ListingEntry` carries a name, a kind, whether it is navigable, its metadata
when that could be read, and a `defect` naming why when it could not. `EntryMetadata`
holds a size, a modification time as signed Unix seconds plus a non-negative
nanosecond remainder, an optional Unix mode, and a symlink's target. A symlink
describes **itself**, as `lstat` and `ls -l` do, and names its target separately; a
dangling link therefore still reports metadata and stays distinguishable from an entry
that could not be stat'd at all. `metadata` and `unix_mode` use explicit presence, so
absent is distinct from zero and an empty file never reads as an unreadable one.

`Walk` recursively traverses the selected-host directory in pull-based,
pre-order depth-first order. The requested root is excluded; direct children
have depth 1. Each entry carries a non-empty root-relative path, its
`ListingEntry` metadata, and an explicit depth. Sibling order is
filesystem-native and unsorted. Directory symlinks are emitted with their
normal navigation metadata but are never followed.

Walk batches are non-empty and contain at most 1024 entries and at most 1 MiB
of encoded protobuf data. A terminal filesystem or cancellation status may
follow valid batches. The client rejects empty batches, missing nested listing
metadata, unknown kinds, zero or out-of-range depth, and empty, absolute, or
parent-containing relative paths; malformed data terminates the stream without
consuming later batches.

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

`Write` is client-streaming and authenticated like every other File Peeker RPC.
Its first frame is exactly one header containing destination directory,
single-component name, and `FAIL_IF_EXISTS` or `REPLACE`. Zero or more
non-empty data frames of at most 64 KiB follow. Exactly one commit frame must
then be followed immediately by end-of-stream. Header plus commit writes a
valid empty file.

Missing payloads, invalid conflict values, misplaced or duplicate headers,
empty or oversized data, frames after commit, and EOF without commit return
`InvalidArgument`. A client source error ends the request without commit; the
server removes its partial file instead of publishing the prefix. The server
returns the absolute selected-host path and byte count only after atomic
publication. Both request decoding and response encoding enforce the 1 MiB
message limit. Dropping or cancelling either side cancels the write and cleans
up incomplete data.

## Status mapping

| Filesystem result | gRPC status |
| --- | --- |
| Missing path | `NotFound` |
| Existing destination under fail-if-exists | `AlreadyExists` |
| Permission failure | `PermissionDenied` |
| Non-directory list or walk target | `FailedPrecondition` |
| Non-file read target | `FailedPrecondition` |
| Invalid path, or one that is not correctly escaped | `InvalidArgument` |
| Service or operation cancellation | `Cancelled` |
| Other filesystem failure | `Internal` |

The client maps these statuses back to the appropriate Rust backend and public
operation error surfaces. API major version 1 remains reported by
`version --format json`.
