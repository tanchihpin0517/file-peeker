# Connection Lifecycle

This document owns local and remote connection startup, transport recovery,
cancellation and shutdown behavior. See [Architecture](architecture.md) for
component responsibilities and [Remote Protocol](protocol.md) for the wire
contract.

Every connection owns one managed server and one authenticated channel to it.
The two targets differ only in the middle of that line.

```text
Local:  Client -> 127.0.0.1:SERVER_PORT -> embedded server
Remote: Client -> SOCKS 127.0.0.1:PROXY -> SSH -> remote loopback server
```

The TUI can open either target interactively. It creates a stable UI row in
`Starting`, records the connection ID as soon as `Client.open` returns, and then
transitions the row to `Ready` or `Failed`. `Ready` means startup succeeded; the
protocol has no heartbeat-backed connected state.

Startup is one shared sequence: start a server somewhere, learn its loopback
port and bearer token, connect a tonic channel through the matching connector,
and run an authenticated health check. Steps 1 and 2 are the only ones that
differ; a failure at any step stops the server that was just started.

## Local startup

1. Bind an ephemeral `127.0.0.1` listener in this process, mint a token, and
   serve on a task. No executable is installed and no child is spawned.
2. The tonic custom connector opens a direct connection to
   `127.0.0.1:SERVER_PORT`.

The loopback port is reachable by any process on the client host for the
connection's lifetime; the 256-bit bearer token is what keeps it private.

## Remote startup

1. Start `ssh -T -D 127.0.0.1:PORT DESTINATION` with piped stdin/stdout. Through
   that shell, ensure the matching server executable exists and then `exec` it
   with `serve`; the SSH stdin becomes the remote server's lease. Parse the
   remote startup record.
2. The tonic custom connector opens a local connection to the SOCKS proxy and
   performs SOCKS5 negotiation for remote `127.0.0.1:SERVER_PORT`.

Tonic multiplexes concurrent RPCs over one channel. If the transport drops,
the interrupted RPC fails; a later RPC may cause tonic to reconnect through a
new SOCKS stream. Interrupted RPCs and their list, walk, read, or write streams
are never automatically replayed or resumed.

## Shutdown

`Client.close` removes the named connection, and `Client.close_all` drains the
registry; both succeed for unknown or already closed IDs. Closing a connection
stops its server, which cancels that server's `FsService` and with it every
active listing, walk, file-read, and file-write stream. Cancellation happens
before the server stops accepting, so an interrupted stream reports cancellation
rather than a transport failure. Incomplete writes remove their sibling partial
files.

Close is terminal in two steps: the registry entry is removed, and then the
connection slot behind it is emptied. Emptying the slot is what rejects a
request that had already reached the slot, so a caller holding a stale ID gets
`connection is closed` rather than a server that is going away.

A local connection cancels its serving task and joins it. A remote connection
drops the channel and the stdin lease; server EOF drives tonic graceful shutdown
on the far side. Either way the client waits up to five seconds, then kills and
reaps while returning a shutdown timeout. Drop uses best-effort non-blocking
cleanup.

In the TUI, a confirmed close first marks the row `Closing` and drops all of its
Browser Contexts. It clears a clipboard sourced from that connection, dismisses
a related overwrite prompt, and aborts and joins any transfer using the
connection before running `Client.close` in a tracked task. The row is removed
when close finishes. A close failure is shown as a persistent UI error rather
than a retryable row because the Client registry entry has already been
consumed. Application shutdown calls `Client.close_all`, which attempts every
remaining close even when one fails and reports the first error.

The protobuf API has no custom Hello, Heartbeat, or Shutdown RPC. Authentication
is sensitive `authorization: Bearer TOKEN` metadata on both FilePeeker and
standard Health requests.
