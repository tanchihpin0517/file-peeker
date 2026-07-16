# File Peeker Protocol v1

Status: design only; not implemented.

This is the private local protocol between the shared client library and its
dedicated host. UIs do not implement this protocol. Both the Rust TUI and a
future native Swift UI use the same client library and its UI-independent
interface. Swift reaches that interface through UniFFI-generated bindings;
UniFFI is not part of the host wire protocol.

Each `BrowserClient` owns one host. They use one long-lived control connection
and one connection per filesystem operation. Each operation connection carries
exactly one request, so messages do not need request IDs or multiplexing.

## Transport

The host listens on one private Unix domain stream socket and accepts the
owning client's control and operation connections. Each message is one UTF-8
JSON object followed by a newline (NDJSON).

- The receiver handles each complete line immediately.
- A message must be no larger than 1 MiB.
- Unknown, malformed, or oversized messages cause the receiver to close the
  connection.
- Extra JSON fields are ignored for forward compatibility.

Paths are absolute UTF-8 strings in v1. The client converts relative input
against its current directory before sending it. The host rejects relative
paths. Supporting non-UTF-8 Unix paths is deferred.

## Connection roles

Every connection starts with `hello` and declares its role.

Control connection:

```json
{"type":"hello","version":1,"role":"control"}
```

Operation connection:

```json
{"type":"hello","version":1,"role":"operation"}
```

The host accepts the version:

```json
{"type":"hello_ok","version":1}
```

Or rejects it and closes the connection:

```json
{"type":"error","code":"unsupported_version","message":"Unsupported protocol version"}
```

No other message may be sent before `hello_ok`.

Exactly one control connection exists. It stays open for the lifetime of
`BrowserClient` and carries no filesystem operations in v1. If it closes, the
host closes all operation connections and exits.

Each operation connection sends exactly one `list` or `get_metadata` request
after `hello_ok`, receives that operation's responses, and then closes. Multiple
operation connections may be active at once.

The control connection must be established first. The host rejects operation
connections received before the control connection or after it has closed.

The host is dedicated to one client and its socket is placed in a private
owner-only directory. Therefore every accepted connection implicitly belongs
to that client; no session token is used.

## List a directory

Request:

```json
{"type":"list","path":"/tmp/example"}
```

The host sends one `entry` for every direct child:

```json
{"type":"entry","path":"/tmp/example/docs","name":"docs","kind":"directory","navigable":true}
```

It then sends exactly one terminal message.

Successful completion:

```json
{"type":"done"}
```

Failure:

```json
{"type":"error","code":"permission_denied","message":"Cannot read directory"}
```

An empty directory produces only `done`. A failure can occur after some
entries; those entries remain valid. The host flushes entries promptly so the
UI can display them before the whole directory has been read.

`kind` is one of:

- `file`
- `directory`
- `symlink`
- `other`

`navigable` is true for directories and for symlinks whose current targets are
directories. `name` is the final path component for display. Clients use
`path`, not `name`, for later operations.

## Get metadata

Metadata exists for development, testing, and future UIs. The v1 terminal UI
does not request or display it.

Request:

```json
{"type":"get_metadata","path":"/tmp/example/docs"}
```

Success:

```json
{"type":"metadata","path":"/tmp/example/docs","kind":"directory","size":96,"readonly":false,"modified":"2026-07-16T12:10:00Z"}
```

`modified` is an RFC 3339 UTC timestamp or `null`. `size` is a non-negative JSON
integer with filesystem-defined meaning for the entry type.

Failure uses the same `error` message as directory listing.

## Errors

Operation errors contain a stable `code` and a human-readable `message`.
Clients use the code for control flow.

| Code | Meaning |
| --- | --- |
| `not_found` | The path does not exist |
| `permission_denied` | OS permissions rejected the operation |
| `not_directory` | A listing path is not a directory |
| `invalid_path` | The supplied path is invalid |
| `io` | Another filesystem I/O error occurred |
| `unsupported_version` | The protocol version is not supported |

An `error` is terminal for the current operation. Framing or message-order
errors close the connection because continuing could misread the stream.

## Stream rules

- An operation connection carries exactly one request.
- `entry` is valid only while handling `list`.
- `metadata` is valid only while handling `get_metadata`.
- The client makes each `entry` available through
  `DirectoryListing.next_entry()`.
- `done` makes the next client read return `None`.
- An operation `error` is returned to the caller.
- Losing an operation connection fails only that operation.
- Closing an operation connection cancels that operation.
- Losing the control connection or host process invalidates the client and all
  active operations.

Closing the control connection is the shutdown mechanism. There is no separate
shutdown request.

## Future compatibility

Changes that break these fields or message sequences require a new protocol
version. Optional fields may be added because receivers ignore unknown fields.

Remote transport is outside v1. Before network use, the design must add
authentication, encryption, authorization, and allowed filesystem roots. The
client interface should remain stable when a remote transport is added so UIs
do not need to learn the wire protocol.
