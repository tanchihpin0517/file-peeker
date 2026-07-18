# File Peeker Protocol v1

File Peeker uses a private NDJSON protocol over local or SSH-forwarded Unix
domain sockets. Protocol v1 is still under development; the streaming listing
sequence replaces the earlier atomic `list_result` shape without compatibility.

Each `Session` owns one long-lived control connection and opens one connection
per filesystem operation. Operation connections carry one request, so the
protocol does not use request IDs or multiplexing.

## Framing and handshakes

Every message is one UTF-8 JSON object followed by `\n`. An encoded message may
not exceed 1 MiB. Malformed, oversized, or out-of-order messages terminate the
operation.

Every connection begins with `hello`:

```json
{"type":"hello","version":1,"role":"control"}
{"type":"hello","version":1,"role":"operation"}
```

The server replies:

```json
{"type":"hello_ok","version":1}
```

or rejects the version with a terminal `error`. The control connection accepts
no messages after its handshake. Closing it shuts down the dedicated server.

## List a directory

The client requests the direct children of one absolute UTF-8 path:

```json
{"type":"list","path":"/tmp/example"}
```

The server enumerates without globally sorting and sends zero or more non-empty
batches:

```json
{"type":"list_batch","entries":[{"name":"docs","kind":"directory","navigable":true}]}
```

Wire entries omit their repeated parent path. The Rust client validates `name`
as one path component and reconstructs the absolute child path before exposing
`DirectoryEntry` to a UI.

Successful enumeration ends explicitly:

```json
{"type":"list_end"}
```

An empty directory sends only `list_end`. EOF before `list_end` is a truncated
operation, never success.

The server targets batches of 64 KiB and flushes at 512 entries or 25 ms after
the first buffered entry, whichever occurs first. The timer is not reset by new
entries. Enumeration pauses while a batch write is pending, providing bounded
backpressure without a prefetch queue.

If enumeration fails after valid entries were buffered, the server flushes that
batch and then sends a terminal error. Clients may therefore retain useful
partial results while marking the listing incomplete.

`kind` is `file`, `directory`, `symlink`, or `other`. `navigable` is true for
directories and symlinks whose current target is a directory. Failed symlink
target resolution makes the symlink non-navigable. Non-UTF-8 filenames are
unsupported and terminate the listing with `invalid_path`.

## Other operations

Current root:

```json
{"type":"current_root"}
{"type":"current_root","path":"/home/example"}
```

Metadata remains reserved:

```json
{"type":"get_metadata","path":"/tmp/example/docs"}
{"type":"metadata","path":"/tmp/example/docs","kind":"directory","size":96,"readonly":false,"modified":null}
```

## Errors and operation rules

```json
{"type":"error","code":"permission_denied","message":"Cannot read directory"}
```

Codes are `not_found`, `permission_denied`, `not_directory`, `invalid_path`,
`io`, and `unsupported_version`. An error is terminal for its operation.

- One operation connection carries one request.
- `list_batch`, `list_end`, and listing errors are valid only after `list`.
- Empty listing batches are invalid.
- Closing an operation connection cancels that operation.
- Multiple operation connections may run concurrently.
- A directory listing is not a filesystem snapshot; changes during enumeration
  have platform-defined visibility.
