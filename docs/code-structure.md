# Code Structure

This document records the intended code structure for File Peeker. It has two
parts:

1. the structure currently adopted by the project, including the pending TUI
   split; and
2. the rules and expected module growth for future features.

The goal is not to make every crate mirror every operation. Each layer should
own one coherent responsibility and expose the smallest useful interface to the
layer above it.

## Part I: Current structure plan

### Responsibility boundaries

| Layer | Owns | Does not own |
| --- | --- | --- |
| `file-peeker-core` | Host-local filesystem behavior, transport-neutral types, streaming and cancellation semantics | Session selection, SSH, gRPC, UniFFI, UI behavior |
| `SessionBackend` | Primitive capabilities against the Session's selected host | Multi-step user workflows or operating-system UI actions |
| `Session` | Public, target-independent workflows and Session lifecycle | gRPC/protobuf details or filesystem implementation details |
| Remote backend modules | One gRPC request/response adapter per backend capability | Public workflow policy |
| Server `ops` | RPC validation, protobuf conversion, transport batching and delegation to core | Client workflows and UI state |
| `session/ffi` | UniFFI-compatible wrappers and error/state adaptation | Native Session behavior |
| TUI | Application state, input routing, rendering and task ownership | Filesystem and transport semantics |

The main boundary is:

```text
UI / FFI
   |
   v
Session workflow
   |
   v
SessionBackend capability
   |-- local  --> FsService --------------------------> host filesystem
   `-- remote --> gRPC adapter --> server op --> FsService --> host filesystem
```

`Session` and `SessionBackend` remain separate. A backend method answers “what
can be done against the selected host?”, while a Session operation answers
“what workflow does the caller want?”. Simple workflows may delegate once;
more involved workflows may compose several backend capabilities plus
client-local behavior.

### Core

Status: implemented.

```text
crates/file-peeker-core/src/
├── lib.rs
├── error.rs
├── fs_service.rs
├── resolve_path.rs
├── read.rs
└── directory/
    ├── mod.rs
    └── list.rs
```

- `lib.rs` is a thin crate facade and re-exports public types.
- `FsService` is the cohesive entry point for filesystem behavior and shared
  cancellation.
- `resolve_path.rs` and `read.rs` hold focused operations that do not yet form
  larger domain families.
- `directory/` is justified because directory behavior already owns shared
  types and a listing operation.
- Tests stay beside the behavior they verify.

The asymmetry is intentional. A `file/` directory should not be created merely
to wrap the single `read.rs` operation. It becomes useful when several
file-oriented operations need shared types or policy.

### Client and Session

Status: implemented.

```text
crates/file-peeker-client/src/session/
├── mod.rs
├── path.rs
├── directory/
│   ├── mod.rs
│   └── list.rs
├── backend/
│   ├── mod.rs
│   ├── local.rs
│   ├── error.rs
│   ├── connection/
│   │   ├── mod.rs
│   │   └── remote.rs
│   └── remote/
│       ├── mod.rs
│       ├── error.rs
│       ├── resolve_path.rs
│       ├── list_dir.rs
│       └── read_file.rs
└── ffi/
    ├── mod.rs
    └── listing.rs
```

- `session/mod.rs` owns Session construction, lifecycle and the private backend
  handle.
- `path.rs` contains native Session path workflows.
- `directory/` contains transport-neutral directory result types and native
  Session directory workflows.
- `backend/mod.rs` defines the private `SessionBackend` capability seam.
- `backend/local.rs` adapts `FsService`; `backend/remote/` contains exact gRPC
  adapters for the same primitive capabilities.
- `backend/connection/` owns remote connection and server-process lifecycle,
  not filesystem operations.
- `ffi/` adapts native APIs for UniFFI. Its sticky listing state is a
  foreign-language boundary concern and should not leak back into the native
  stream.

`read_file` currently remains backend-only. It should gain a Session-level
operation only when there is a caller-facing file workflow, rather than being
exposed solely to make the layers symmetrical.

### Server

Status: implemented.

```text
crates/file-peeker-server/src/
├── lib.rs
├── main.rs
├── server.rs
└── ops/
    ├── mod.rs
    ├── status.rs
    ├── list.rs
    └── read.rs
```

- `main.rs` is the thin CLI entry point.
- `server.rs` owns listener setup, authentication, health reporting, startup,
  shutdown and the `serve` lifecycle.
- `ops/mod.rs` is the tonic service adapter and keeps trivial delegation, such
  as path resolution, inline.
- `ops/list.rs` and `ops/read.rs` own behavior-heavy streaming conversion and
  batching.
- `ops/status.rs` owns the status-to-gRPC error mapping shared by server
  operations.

There is no server backend trait. The server has one real filesystem
implementation, `FsService`; adding an interface there now would create a seam
without variation.

### TUI

Status: the ownership model is implemented, but the module split below is the
next planned refactor. At present, `App` and rendering still live in `main.rs`,
while `BrowserContext` has its own module.

```text
crates/file-peeker-tui/src/
├── main.rs
├── browser_context.rs
└── app/
    ├── mod.rs
    └── view.rs
```

- `main.rs` should retain CLI parsing, terminal setup/restore, the event loop
  and direct key-to-command mapping.
- `app/mod.rs` should own `App`, application lifecycle, context routing and
  commands acting on the active context.
- `app/view.rs` should render application state without owning async tasks or
  mutating browser state.
- `browser_context.rs` should remain the deep browsing module: resolved Session,
  current path, entries, selection, listing generation, task cancellation,
  stale-result rejection and terminal listing state.

This split keeps high-level orchestration visible without fragmenting
`BrowserContext` into state, event and task files that would need to understand
the same invariants.

### Placement rule for a new operation

Before adding a file, identify the operation's semantic owner:

1. Put host-local filesystem mechanics in core.
2. Put a primitive selected-host capability in `SessionBackend`.
3. Implement local and remote adapters only when that capability must work on
   both targets.
4. Put multi-step behavior and client-machine side effects in `Session`.
5. Put protobuf conversion and wire batching in the remote adapter and server
   `ops`.
6. Add an FFI or UI adapter only when that boundary has a real consumer.

A public Session workflow does not require an identically named core, backend,
RPC and FFI operation. Layers should mirror semantics only where the semantics
are actually shared.

## Part II: Future feature structure plan

### Growth rules

- Prefer a new function when behavior has different semantics. Do not hide
  recursive traversal behind `list_dir(recursive: bool)`.
- Add an options type only when at least one real policy choice exists. Avoid
  empty or speculative option structs.
- Create a domain directory when multiple operations share vocabulary, types
  or invariants. Uneven module depth is acceptable.
- Extract shared batching, conversion or task helpers after a second caller
  demonstrates the duplication and its common invariant.
- Add a trait only when there are real implementations to vary, or a boundary
  must be substituted in tests.
- Keep streaming pull-based and preserve partial results followed by a terminal
  error unless a feature explicitly requires different semantics.
- Avoid adding FFI and UI surface area until a concrete consumer needs it.

### Recursive directory traversal

Recursive traversal should be a separate `walk_dir` feature rather than a mode
of `list_dir`. Listing one directory and traversing a tree have different cost,
ordering, cancellation and error semantics.

Expected structure:

```text
crates/file-peeker-core/src/directory/
├── mod.rs
├── list.rs
└── walk.rs

crates/file-peeker-client/src/session/
├── directory/
│   ├── mod.rs
│   ├── list.rs
│   └── walk.rs
└── backend/
    ├── mod.rs
    ├── local.rs
    └── remote/
        ├── mod.rs
        └── walk_dir.rs

crates/file-peeker-server/src/ops/
├── mod.rs
└── walk.rs
```

The first version should:

- expose `FsService::walk_dir` and a distinct `WalkStream`;
- yield a `WalkEntry` containing a relative path, entry metadata and depth;
- exclude the requested root and assign depth `1` to direct children;
- traverse depth-first using an explicit stack;
- emit symlinks but never follow them;
- avoid global collection and sorting;
- preserve already-yielded entries when a terminal error or cancellation
  occurs; and
- omit `WalkOptions` until a concrete option such as maximum depth or symlink
  policy is actually supported.

Remote traversal should use a dedicated streaming `Walk` RPC because its result
shape and server execution differ from `List`. The server may share a batching
helper with listing only after both implementations prove the same byte and
entry-count invariants. FFI and TUI adapters can be added later when either UI
needs recursive results.

### Local materialization and opening a file

Opening a selected-host file is a Session workflow:

```text
Session::open_file(path)
    |
    v
SessionBackend::cache_file(path) -> local filesystem path
    |-- local backend: validate/resolve and return the existing local path
    `-- remote backend: stream the file into a managed local cache
    |
    v
Session invokes the client operating system's open action
```

`cache_file` belongs at the backend seam because its result depends on whether
the selected host is local or remote. The operating-system open action belongs
to Session because it is a workflow and a side effect on the client machine.
Neither core nor the remote server should launch a client-side application.

When this feature is added, several file workflows will exist, so a file domain
becomes justified:

```text
crates/file-peeker-client/src/session/
├── file/
│   ├── mod.rs
│   ├── read.rs
│   ├── cache.rs
│   └── open.rs
└── backend/
    └── remote/
        └── cache_file.rs
```

The cache contract must define path lifetime, cleanup, replacement of stale
content, filename handling and failure behavior. The remote implementation can
initially compose the existing streaming read capability; a new server RPC is
unnecessary unless remote materialization later needs semantics that `Read`
cannot provide.

### Future server growth

- Keep small validation/delegation methods in `ops/mod.rs`.
- Give each behavior-heavy streaming RPC its own `ops/<operation>.rs` module.
- Keep transport limits and protobuf conversion on the server side; core
  streams should not inherit gRPC message-size policy.
- Introduce shared operation infrastructure only after at least two operations
  need the same invariant.
- Do not add a server repository/backend abstraction while `FsService` remains
  the sole implementation.

### Future TUI growth

- Add `app/input.rs` and an `Action` type only when input gains modes,
  configurable bindings or a command palette.
- Add a `locations/` or sidebar domain only when it owns state and actions, not
  merely because a second panel is rendered.
- Keep `BrowserContext` cohesive until recursive traversal, search or another
  task family proves a smaller reusable task abstraction.
- UI modules may consume `walk_dir` or `open_file`, but they should not recreate
  traversal, caching or transport policy.

### Checklist for adding a feature

1. Define the caller-visible semantics, especially streaming, partial-result,
   cancellation and error behavior.
2. Place the behavior at its semantic owner rather than copying the operation
   across all layers.
3. Add core types and facade re-exports only when the behavior is
   transport-neutral.
4. Extend `SessionBackend` only for a selected-host primitive.
5. Add local and remote implementations, plus protocol and server work, only
   where remote execution requires them.
6. Add Session composition for caller workflows.
7. Add FFI and UI adapters only for real consumers.
8. Keep tests beside the layer-specific invariant they verify.
9. Update this document when the new feature changes a responsibility boundary.
