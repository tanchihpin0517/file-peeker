# v2 Documentation

What v2, at the repository root, is built from. These documents describe what
exists; the proposal under [design/](design/README.md) owns what is planned. The
shipping crate's documents are in [old/docs/](../old/docs/README.md).

| Document | Covers |
| --- | --- |
| [Task architecture](task-architecture.md) | The task runtime: its parts, the lifecycle, kinds and contexts, the tree, resources, admission, cancellation, progress, retention, concurrency |
| [Task pipeline](task-pipeline.md) | One task end to end: the API, the runtime, `Watch`, the wire, the client, the sidebar, and shutdown |
| [Machines](machines.md) | Saved SSH machines: probing, installing by build profile, the bridge, shared SSH, the terminal UI's machine list, `--machine`, releases |
| [v2 architecture](design/v2-architecture.md) | The rewrite's proposal and build order |
| [Design proposals](design/README.md) | The proposal and the layout and palette galleries |
