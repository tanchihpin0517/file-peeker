# File Peeker Documentation

This directory is the entry point for File Peeker's maintained design and usage
documentation.

## Reading paths

### Understand the system

1. [Architecture](architecture.md) explains the runtime model, terminology and
   responsibility boundaries.
2. [Session lifecycle](session-lifecycle.md) covers local and remote startup,
   reconnection, cancellation and shutdown.
3. [Remote protocol](protocol.md) defines the gRPC wire contract.

### Add or change a feature

1. [Code structure](code-structure.md) records the current module layout and
   placement rules.
2. Read the relevant operation document, such as
   [Open file](operations/open-file.md).
3. Use [Client API](api.md) to verify the caller-facing surface.

### Work on a frontend or command-line tool

- [TUI implementation](tui.md)
- [Client and UI state ownership](state-ownership.md)
- [Client test CLI](client-cli.md)

## Document responsibilities

Each fact should have one detailed owner. Other documents should summarize it
and link to that owner rather than duplicate implementation details.

| Document | Owns |
| --- | --- |
| [Architecture](architecture.md) | System vocabulary, component boundaries and high-level data flow |
| [Code structure](code-structure.md) | Current module placement, growth rules and implemented placement examples |
| [Session lifecycle](session-lifecycle.md) | Startup, reconnection, cancellation and shutdown |
| [Remote protocol](protocol.md) | RPCs, protobuf shapes, wire limits and gRPC status mapping |
| [Client API](api.md) | Caller-visible Rust and UniFFI surface |
| [Open file](operations/open-file.md) | Open-file workflow, staging, cache location, failure handling and retention |
| [TUI implementation](tui.md) | TUI state, tasks, events, input and rendering behavior |
| [State ownership](state-ownership.md) | Cross-client and cross-UI ownership comparison |
| [Client test CLI](client-cli.md) | Test CLI commands and output behavior |

## Writing conventions

- **Current behavior** uses present tense and describes implemented behavior.
- **Design rules** use normative language only where future code is expected to
  follow an established boundary.
- **Future possibilities** are labeled explicitly and do not appear as current
  behavior.
- Use the terms defined in [Architecture](architecture.md) consistently,
  especially *client host*, *selected host* and *server host*.

