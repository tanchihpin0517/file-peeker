# Session Lifecycle

This document owns local and remote Session startup, transport recovery,
cancellation and shutdown behavior. See [Architecture](architecture.md) for
component responsibilities and [Remote Protocol](protocol.md) for the wire
contract.

Every `Session` owns either a native filesystem core or a managed remote server
connection.

```text
Local:  Session -> in-process filesystem core
Remote: Session -> SOCKS 127.0.0.1:PROXY -> SSH -> remote loopback server
```

## Local startup

1. Construct an in-process `FsService`.
2. Retain it as the Session's local backend.
3. Return the Session without installing a server, spawning a child, opening a
   port, or performing a health check.

## Remote startup

1. Start `ssh -T -D 127.0.0.1:PORT DESTINATION` with piped stdin/stdout.
2. Through that shell, ensure the matching server executable exists and then
   `exec` it with `serve`; the SSH stdin becomes the remote server's lease.
3. Parse the remote startup record.
4. The tonic custom connector opens a local connection to the SOCKS proxy,
   performs SOCKS5 negotiation for remote `127.0.0.1:SERVER_PORT`, and runs the
   authenticated health check over that HTTP/2 channel.

Tonic multiplexes concurrent RPCs over one channel. If the transport drops,
the interrupted RPC fails; a later RPC may cause tonic to reconnect through a
new SOCKS stream. Interrupted RPCs and their list, walk, or read streams are
never automatically replayed or resumed.

## Shutdown

`Client.close_session` removes the retained Session; native `Session.close` and
UniFFI `Session.close_uniffi` are idempotent without unregistering it. Closing a
local Session cancels its core service and active listing, walk, and file-read
streams. Closing a remote Session drops the channel and stdin lease. Server EOF
cancels active listing, walk, and file-read streams and drives tonic graceful
shutdown. The client waits up to five seconds for a managed remote child, then
kills and reaps it while returning a shutdown timeout. Drop uses best-effort
non-blocking cleanup.

The protobuf API has no custom Hello, Heartbeat, or Shutdown RPC. Authentication
is sensitive `authorization: Bearer TOKEN` metadata on both FilePeeker and
standard Health requests.
