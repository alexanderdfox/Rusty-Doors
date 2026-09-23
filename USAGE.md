# Usage

## Build

```bash
cargo build --release
# binary: target/release/doors
```

## Key generation

Create a strong shared secret key (recommended once per deployment):

```bash
doors keygen --out ~/.doors/my.key
```

Options:

| Flag          | Default     | Description                          |
|---------------|-------------|--------------------------------------|
| `--out <PATH>`| `doors.key` | Output path for the key file         |
| `--bits <N>`  | `256`       | Bit length of the secret odd integer |

The key file is written with mode `0600` and looks like:

```
# doors binary-reciprocal key
# keep this file secret
n=<large-odd-integer>
bits=256
```

You can also set the environment variable for convenience:

```bash
export DOORS_KEY=~/.doors/my.key
```

## Server

Create a door and attach it to a path (the equivalent of `door_create` + `fattach`).

```bash
doors server [OPTIONS]
```

### Options

| Flag            | Default            | Description                                      |
|-----------------|--------------------|--------------------------------------------------|
| `--path <PATH>` | `/tmp/hello.door`  | Filesystem path that names the door              |
| `--key <PATH>`  | *(none)*           | Path to key file (or use `DOORS_KEY` env var)    |

### Examples

```bash
# Unauthenticated (original behaviour)
doors server

# Authenticated server
doors server --path /tmp/secure.door --key ~/.doors/my.key

# Using environment variable
export DOORS_KEY=~/.doors/my.key
doors server --path /tmp/secure.door
```

The server runs forever until you hit Ctrl-C.

When it starts you will see:

```
door attached at /tmp/secure.door (authenticated)
```

or

```
door attached at /tmp/hello.door (unauthenticated)
```

## Client

Call the door (the equivalent of `door_call`).

```bash
doors client [OPTIONS]
```

### Options

| Flag              | Default            | Description                                      |
|-------------------|--------------------|--------------------------------------------------|
| `--path <PATH>`   | `/tmp/hello.door`  | Path of the door to call                         |
| `--msg <STRING>`  | `Hello, World!`    | Data to send                                     |
| `-n, --count <N>` | `1`                | Number of concurrent calls                       |
| `-m, --max-jobs <N>` | `8`             | Max concurrent jobs                              |
| `--auth`          |                    | Perform authenticated handshake first            |
| `--key <PATH>`    | *(none)*           | Path to key file (required with `--auth`)        |

### Examples

#### Simple unauthenticated call

```bash
doors client --path /tmp/hello.door --msg "knock knock"
```

#### Authenticated call

```bash
doors client --path /tmp/secure.door --key ~/.doors/my.key --auth
```

After successful authentication you can still send a normal message:

```bash
doors client --path /tmp/secure.door --key ~/.doors/my.key --auth --msg "secret payload"
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

## Typical authenticated workflow

```bash
# One-time key generation
mkdir -p ~/.doors
./target/release/doors keygen --out ~/.doors/prod.key

# Terminal 1 – server
./target/release/doors server --path /tmp/secure.door --key ~/.doors/prod.key

# Terminal 2 – client
./target/release/doors client --path /tmp/secure.door --key ~/.doors/prod.key --auth
```

## Authentication protocol (for reference)

1. Client → `AUTH:CHAL:<32-byte-hex-nonce>`
2. Server → `AUTH:CHAL:<remainder-hex>:<bits>`
3. Client computes the next `bits` of the binary expansion starting from the remainder and replies  
   `AUTH:RESP:<nonce>:<bitstring>`
4. Server replies `AUTH:OK` or `AUTH:ERR:…`

The remainder is derived from the nonce + the secret key, so the challenge is bound to both the session and the key.

## Notes

- Unauthenticated mode remains fully compatible with the original revolving-doors style usage.
- Always keep key files private (`chmod 600`).
- For production, generate a fresh 256-bit (or larger) key and never commit it to version control.
```
