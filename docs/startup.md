# Startup routine

Every `Session` owns one local or remote Rust server, one memory-only token, one
reconnectable gRPC channel, and one stdin lifetime lease.

```text
Local:  Session -> HTTP/2 127.0.0.1:PORT -> server
Remote: Session -> SOCKS 127.0.0.1:PROXY -> SSH -> remote loopback server
```

## Local startup

1. Reuse or install the matching executable below
   `~/.file-peeker/servers/VERSION`.
2. Spawn `file-peeker-server serve` with piped stdin and stdout.
3. Parse `FILE_PEEKER_SERVER_STARTUP={port,token}` from stdout.
4. Open an eager tonic channel to the loopback port.
5. Send an authenticated `grpc.health.v1.Health/Check`; return the Session only
   when the service reports `SERVING`.

The server chooses an ephemeral port and accepts only IPv4 loopback traffic.
The client uses a five-second connect/health timeout and enables HTTP/2 PING
every 15 idle seconds with a five-second timeout.

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
new SOCKS stream. RPCs and listings are never automatically replayed.

## Shutdown

`Client.close_session` removes the retained Session; native `Session.close` and
UniFFI `Session.close_uniffi` are idempotent without unregistering it. Explicit close drops the channel and stdin
lease. Server EOF cancels active listings and drives tonic graceful shutdown.
The client waits up to five seconds for the managed child, then kills and reaps
it while returning a shutdown timeout. Drop uses best-effort non-blocking
kill/reap cleanup.

The protobuf API has no custom Hello, Heartbeat, or Shutdown RPC. Authentication
is sensitive `authorization: Bearer TOKEN` metadata on both FilePeeker and
standard Health requests.
