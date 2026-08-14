# File Peeker Documentation

This directory is the entry point for File Peeker's maintained design and usage
documentation.

## Reading paths

### Understand the system

1. [Architecture](architecture.md) explains the runtime model, terminology and
   responsibility boundaries.
2. [Connection lifecycle](connection-lifecycle.md) covers local and remote startup,
   reconnection, cancellation and shutdown.
3. [Remote protocol](protocol.md) defines the gRPC wire contract.

### Add or change a feature

1. [Code structure](code-structure.md) records the current module layout and
   placement rules.
2. Read the relevant operation document, such as
   [Copy file](operations/copy-file.md).
3. Use [Client API](api.md) to verify the caller-facing surface.

### Work on a frontend or command-line tool

- [TUI implementation](tui.md)
- [Client and UI state ownership](state-ownership.md)
- [Client test CLI](client-cli.md)
- [Layout and palette galleries](design/README.md), browsable proposals for
  making a frontend's arrangement and its colors selectable

## Document responsibilities

Each fact should have one detailed owner. Other documents should summarize it
and link to that owner rather than duplicate implementation details.

| Document | Owns |
| --- | --- |
| [Architecture](architecture.md) | System vocabulary, component boundaries and high-level data flow |
| [Code structure](code-structure.md) | Current module placement, dependency direction, binary targets and features, growth rules and implemented placement examples |
| [Connection lifecycle](connection-lifecycle.md) | Startup, reconnection, cancellation and shutdown |
| [Remote protocol](protocol.md) | RPCs, protobuf shapes, wire limits and gRPC status mapping |
| [Client API](api.md) | Caller-visible Rust surface |
| [Copy file](operations/copy-file.md) | Connection-to-connection routes, streaming, conflicts, atomic publication and cancellation |
| [TUI implementation](tui.md) | TUI state, tasks, events, input and rendering behavior |
| [Layout and palette galleries](design/README.md) | The shared layout-and-palette seam, and how the two frontends' galleries differ |
| [TUI layout and palette gallery](design/tui-layouts.md) | The TUI's ten layouts, ten palettes and terminal-size rungs |
| [GUI layout and palette gallery](design/gui-layouts.md) | The desktop front end's ten layouts, ten palettes and window-size rungs |
| [TUI layout and palette system](design/tui-layout-spec.md) | The TUI proposal's implementation contract |
| [GUI layout and palette system](design/gui-layout-spec.md) | The GUI proposal's implementation contract: types, component metrics, palette values, build order |
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
