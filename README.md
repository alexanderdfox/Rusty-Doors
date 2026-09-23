# doors

**Solaris/illumos Doors IPC re-created for Linux & macOS**  
with a production-ready binary-reciprocal key/lock authentication system.

A single-file Rust implementation of the classic Solaris Doors inter-process communication mechanism, inspired by [robertdfrench/revolving-doors](https://github.com/robertdfrench/revolving-doors) and structured in the same spirit as [alexanderdfox/AIP](https://github.com/alexanderdfox/AIP).

Doors let a thread in a client process call a function that lives in a server process. The runtime automatically manages handler tasks on the server side. This is a lightweight, synchronous RPC that feels almost like a local function call.

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
- **Binary-reciprocal key/lock authentication**
  - Shared secret is a large odd integer \(n > 1\)
  - Challenge-response based on the binary expansion of \(1/n\)
  - Period = multiplicative order of 2 modulo \(n\)
  - Easy key generation and key-file workflow
  - Secrets never appear on the command line

## Quick start

```bash
# Build
cargo build --release

# 1. Generate a strong key (once)
./target/release/doors keygen --out ~/.doors/my.key

# 2. Start the authenticated door server
./target/release/doors server --path /tmp/secure.door --key ~/.doors/my.key

# 3. Authenticated client call
./target/release/doors client --path /tmp/secure.door --key ~/.doors/my.key --auth
```

You can also set the environment variable once:

```bash
export DOORS_KEY=~/.doors/my.key
./target/release/doors server --path /tmp/secure.door
./target/release/doors client --path /tmp/secure.door --auth
```

Unauthenticated mode (original behaviour) still works if you omit `--key` / `--auth`.

## Authentication design

The lock is based on the binary long division of \(1/n\) where \(n\) is an odd integer greater than 1.

- Both sides share the same secret key file (containing \(n\)).
- Client sends a random nonce → `AUTH:CHAL:<nonce>`.
- Server replies with a challenge remainder derived from the nonce + key.
- Client continues the binary division from that remainder and returns the next 256 bits (default).
- Server verifies with a constant-time comparison.

This gives a clean challenge-response protocol whose hardness rests on the size of \(n\) and the order of 2 modulo \(n\).

### Key file format

```
# doors binary-reciprocal key
# keep this file secret
n=115792089237316195423570985008687907853269984665640564039457584007913129639937
bits=256
```

Generated keys are written with mode `0600`.

## Mapping to original Solaris Doors

| Original (illumos)              | This implementation                  |
|--------------------------------|--------------------------------------|
| `door_create(&fn, cookie, …)`  | server registers `door_procedure`    |
| `fattach(door_fd, path)`       | `UnixListener::bind(path)`           |
| `door_call(fd, &door_arg_t)`   | `door_call(path, data)`              |
| automatic server threads       | `tokio::spawn` per incoming connection |
| `door_return(data, size, …)`   | JSON response written back on the socket |
| path as name space             | filesystem Unix-domain socket        |
| (new) authentication           | binary-reciprocal challenge-response |

## Project layout

```
src/
  main.rs          # everything – single file by design
Cargo.toml
README.md
USAGE.md           # detailed command reference
```

The entire implementation lives in one Rust source file so it is easy to read, copy, and experiment with – exactly the spirit of the original revolving-doors tutorial.

## Requirements

- Rust 1.70+
- Tokio (async runtime)
- clap, serde, bytes, anyhow, rand, hex, sha2, num-bigint, num-traits, num-integer

## License

MIT (same spirit as the revolving-doors tutorial and AIP)

## Credits

- [robertdfrench/revolving-doors](https://github.com/robertdfrench/revolving-doors) – the best public tutorial on the real Doors API
- [alexanderdfox/AIP](https://github.com/alexanderdfox/AIP) – control-flow and CLI shape
- Original Solaris Doors design by Sun Microsystems / Spring OS team
- Binary-reciprocal authentication designed around the classic long-division expansion of \(1/n\) for odd \(n > 1\)
