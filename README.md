# File Peeker

File Peeker is a native macOS file browser with SwiftUI and Ratatui frontends
backed by a shared Rust client and filesystem server.

The client API and test CLI can install, start, and own a local server or
connect to one over SSH. Both UIs start a local session, discover the server's
current root, and display that directory's entries as bounded gRPC batches.
The initial UI is deliberately read-only and does not navigate or open files.
In the TUI, press `R` to clear and refresh the Home listing.

## Build

XcodeGen and Xcode are required.

```text
make xcode-build
scripts/run-app.sh
```

Generated Xcode projects and DerivedData are not committed.

## Documentation

- [Architecture](docs/architecture.md)
- [TUI implementation](docs/tui.md)
- [Client and UI state ownership](docs/state-ownership.md)

## License

Licensed under the Apache License, Version 2.0. Contributors retain copyright
in their contributions while licensing them under Apache-2.0. See [LICENSE](LICENSE)
and [NOTICE](NOTICE).

Copyright 2026 Chih-Pin Tan.
