# File Peeker

File Peeker is a native macOS file browser with SwiftUI and Ratatui frontends
backed by a shared Rust client and filesystem core.

Local sessions run the filesystem core directly inside the application and read
the client host's filesystem. Remote sessions install and start the matching
server over SSH; that server runs the same core against the remote host's local
filesystem and returns results over authenticated gRPC. SwiftUI discovers and
displays the selected host's current root. The TUI opens an in-app help screen
when started without a path; pass a path to stream that directory's entries.
In the TUI, use `j`/`k` to select entries, `l` to enter a navigable directory,
`h` to leave it, and `R` to clear and refresh the active listing.

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
- [TUI implementation](docs/tui.md)
- [Client and UI state ownership](docs/state-ownership.md)

## License

Licensed under the Apache License, Version 2.0. Contributors retain copyright
in their contributions while licensing them under Apache-2.0. See [LICENSE](LICENSE)
and [NOTICE](NOTICE).

Copyright 2026 Chih-Pin Tan.
