# Usage

## Build

```bash
cargo build --release
# binary: target/release/doors
```

## Server

Create a door and attach it to a path (the equivalent of `door_create` + `fattach`).

```bash
doors server [OPTIONS]
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--path <PATH>` | `/tmp/hello.door` | Filesystem path that names the door |
| `--cookie <STRING>` | *(none)* | Optional opaque cookie passed to the door procedure |

### Examples

```bash
# Basic server
doors server

# Custom path
doors server --path /tmp/my.door

# With a cookie (available inside the door procedure)
doors server --path /tmp/hello.door --cookie "session-42"
```

The server runs forever until you hit Ctrl-C.  
When it starts you will see:

```
door attached at /tmp/hello.door (sleeping forever, Ctrl-C to stop)
```

## Client

Call the door (the equivalent of `door_call`).

```bash
doors client [OPTIONS]
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--path <PATH>` | `/tmp/hello.door` | Path of the door to call |
| `--msg <STRING>` | `Hello, World!` | Data sent to the door procedure |
| `-n, --count <N>` | `1` | Number of concurrent calls |
| `-m, --max-jobs <N>` | `8` | Maximum simultaneous outstanding calls |

### Examples

#### Single call

```bash
doors client --msg "Hello from client"
```

Output:

```
[0] Well, hello to you too! You said: Hello from client
```

#### Many concurrent calls (AIP-style)

```bash
doors client -n 20 -m 4 --msg "ping"
```

You will see interleaved progress lines:

```
[0] started
[1] started
…
status: running=4 done=0 queued=16
[3] Well, hello to you too! You said: ping
…
status: running=0 done=20 queued=0
```

The process exits with code `0` if every call succeeded, `1` otherwise.

## Typical workflow

```bash
# Terminal 1
./target/release/doors server --path /tmp/hello.door

# Terminal 2
./target/release/doors client --path /tmp/hello.door --msg "knock knock"
./target/release/doors client -n 50 -m 8 --msg "load test"
```

## Notes

- The server must be running before any client can connect.
- On Linux the path is a normal filesystem Unix socket; on macOS the same is true.
- If a previous server left a stale socket file, the new server automatically removes it.
- The door procedure currently lives inside the binary (`door_procedure`).  
  Changing its behaviour requires a rebuild (this is intentional for the single-file tutorial style).
