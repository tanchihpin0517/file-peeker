# File Peeker

File Peeker is a native macOS file browser with SwiftUI and Ratatui frontends
backed by a shared Rust client and filesystem core.

Local sessions run the filesystem core directly inside the application and read
the client host's filesystem. Remote sessions install and start the matching
server over SSH; that server runs the same core against the remote host's local
filesystem and returns results over authenticated gRPC. SwiftUI discovers and
displays the selected host's current root. The TUI starts with an interactive
Session Manager; passing a path still immediately opens that path in a local
Session. Use `a` in Session Manager to add local or SSH Sessions and `n` to add
a browser tab. Use `[`/`]` to switch tabs, `x` to close one, and `S` to return
to Session Manager. Within a browser, `j`/`k` selects entries, `l` enters a
navigable directory, `h` leaves it, `o` expands or opens, and `R` refreshes.
Press `y` on a regular file and `p` in any browser to stream it between
Sessions. Remote-to-local copy/paste is the download workflow; an existing
destination requires explicit overwrite confirmation.

## Build

XcodeGen and Xcode are required.

```text
make xcode-build
scripts/run-app.sh
```

Generated Xcode projects and DerivedData are not committed.

## Documentation

- [Documentation index](docs/README.md)
- [Architecture](docs/architecture.md)
- [Code structure](docs/code-structure.md)
- [Copy file](docs/operations/copy-file.md)
- [TUI implementation](docs/tui.md)
- [Client and UI state ownership](docs/state-ownership.md)

## License

Licensed under the Apache License, Version 2.0. Contributors retain copyright
in their contributions while licensing them under Apache-2.0. See [LICENSE](LICENSE)
and [NOTICE](NOTICE).

Copyright 2026 Chih-Pin Tan.
