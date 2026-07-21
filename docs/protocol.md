# File Peeker Protocol v1

File Peeker uses private NDJSON over IPv4 loopback TCP. Local sessions connect
directly; SSH sessions connect to the same remote loopback endpoint through an
OpenSSH SOCKS5 proxy. Every filesystem operation uses a distinct TCP
connection; heartbeat and shutdown share one persistent control connection.

## Startup and authentication

`file-peeker-server serve` binds `127.0.0.1:0`, generates a 256-bit token, and
prints one prefixed JSON record before keeping stdout silent:

```text
FILE_PEEKER_SERVER_STARTUP={"port":43827,"token":"<64 lowercase hexadecimal characters>"}
```

The launcher keeps server stdin open as a lifetime lease. EOF stops the server
and cancels active operations. Tokens are session-specific, memory-only, and
must not appear in arguments, environment, files, diagnostics, or errors.

Every connection begins with an authentication frame:

```json
{"type":"auth","token":"<session token>"}
```

Successful authentication is silent. An invalid or missing token receives a
generic `authentication_failed` error and only that connection is closed.

After authentication, the next frame selects the connection behavior. A
filesystem operation connection carries exactly one request and then closes.
The persistent control connection begins with:

```json
{"type":"hello","version":1}
{"type":"hello_ok","version":1}
```

Unsupported protocol versions are rejected. Messages are UTF-8 JSON followed
by `\n`.

## Heartbeat

```json
{"type":"heartbeat"}
{"type":"heartbeat_ok"}
```

Heartbeat runs on the authenticated control connection. Shutdown uses the same
connection and receives `{"type":"shutdown_ok"}` before the server exits.

## List a directory

```json
{"type":"list","path":"/tmp/example"}
```

The path may be absolute, `~`, or start with `~/`. The server resolves tilde
paths from its own `HOME`, so a remote request uses the remote user's home
directory. Other relative paths and named-user forms such as `~alice` are
invalid.

The server sends zero or more non-empty batches followed by `list_end`:

```json
{"type":"list_batch","entries":[{"name":"docs","kind":"directory","navigable":true}]}
{"type":"list_end"}
```

Wire entries omit their repeated parent path. The client validates each name as
one path component and reconstructs the child path. EOF before
`list_end` is failure. The server targets 128 KiB batches and flushes at 512
entries or 25 ms after the first buffered entry. Errors may follow valid
batches, allowing callers to retain partial results.

`kind` is `file`, `directory`, `symlink`, or `other`. `navigable` is true for
directories and symlinks whose current target is a directory.

## Other operations and errors

```json
{"type":"current_root"}
{"type":"current_root","path":"/home/example"}
```

Metadata remains reserved. Normal operation error codes are `not_found`,
`permission_denied`, `not_directory`, `invalid_path`, and `io`. These terminate
only their operation. Authentication, protocol, heartbeat, SOCKS, or TCP
failures are fatal to the client session; there is no automatic reconnection.

The client permits 64 concurrent operations per session plus a reserved
heartbeat connection. The server permits at most 128 concurrent connections.
