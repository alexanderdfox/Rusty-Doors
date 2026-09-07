# doors

**Solaris/illumos Doors IPC re-created for Linux & macOS**

A single-file Rust implementation of the classic Solaris Doors inter-process communication mechanism, inspired by the excellent tutorial [robertdfrench/revolving-doors](https://github.com/robertdfrench/revolving-doors) and structured in the same spirit as [alexanderdfox/AIP](https://github.com/alexanderdfox/AIP).

Doors let a thread in a client process call a function that lives in a server process. The kernel (or, in this port, the runtime) automatically manages handler threads/tasks on the server side. This is a lightweight, synchronous RPC that feels almost like a local function call.

Because native `door_create` / `door_call` / `fattach` only exist on Solaris/illumos, this project provides a clean userspace equivalent that works on:

- Linux
- macOS

using Unix-domain sockets as the naming namespace (the closest practical analogue to attaching a door to a filesystem path).

## Features

- `door_create`-style server that registers a handler procedure
- `fattach`-style binding to a filesystem path
- `door_call`-style synchronous client RPC
- Automatic handler-task management (mirrors the original automatic thread pool)
- Concurrent client runner with hard max-jobs limit (AIP-style)
- Simple length-prefixed JSON wire protocol
- Optional opaque “cookie” passed to the door procedure
- Zero external services required

## Quick start

```bash
# Build
cargo build --release

# Terminal 1 – start the door server
./target/release/doors server --path /tmp/hello.door

# Terminal 2 – call it
./target/release/doors client --path /tmp/hello.door --msg "Hello, World!"
```

See [USAGE.md](USAGE.md) for the full command reference and examples.

## Mapping to original Solaris Doors

| Original (illumos)               | This implementation                          |
|----------------------------------|----------------------------------------------|
| `door_create(&fn, cookie, flags)`| server registers `door_procedure`            |
| `fattach(door_fd, path)`         | `UnixListener::bind(path)`                   |
| `door_call(fd, &door_arg_t)`     | `door_call(path, data)`                      |
| automatic server threads         | `tokio::spawn` per incoming connection       |
| `door_return(data, size, …)`     | JSON response written back on the socket     |
| path as name space               | filesystem Unix-domain socket                |

## Project layout

```
src/
  main.rs   # everything – single file by design
Cargo.toml
README.md
USAGE.md
```

The entire implementation lives in one Rust source file so it is easy to read, copy, and experiment with – exactly the spirit of the original revolving-doors tutorial.

## Requirements

- Rust 1.70+
- Tokio (async runtime)
- clap, serde, bytes, anyhow

## License

MIT (same spirit as the revolving-doors tutorial and AIP)

## Credits

- [robertdfrench/revolving-doors](https://github.com/robertdfrench/revolving-doors) – the best public tutorial on the real Doors API
- [alexanderdfox/AIP](https://github.com/alexanderdfox/AIP) – control-flow and CLI shape
- Original Solaris Doors design by Sun Microsystems / Spring OS team
