//! doors – Solaris/illumos Doors IPC + secure binary-reciprocal key/lock
//!
//! Easy workflow:
//!   doors keygen --out my.key          # create a strong key
//!   doors server --key my.key          # start authenticated door
//!   doors client --key my.key --auth   # authenticated call
//!
//! The key is a large odd integer. Authentication uses the binary expansion
//! of a challenge remainder divided by the key (period = ord_key(2)).

use anyhow::{bail, Context, Result};
use bytes::{BufMut, BytesMut};
use clap::{Parser, Subcommand};
use num_bigint::BigUint;
use num_integer::Integer;
use num_traits::{One, Zero};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DoorRequest {
	data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DoorResponse {
	data: String,
	error: Option<String>,
}

async fn write_msg<T: Serialize>(stream: &mut UnixStream, msg: &T) -> Result<()> {
	let json = serde_json::to_vec(msg)?;
	let mut buf = BytesMut::with_capacity(4 + json.len());
	buf.put_u32(json.len() as u32);
	buf.extend_from_slice(&json);
	stream.write_all(&buf).await?;
	Ok(())
}

async fn read_msg<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T> {
	let mut len_buf = [0u8; 4];
	stream.read_exact(&mut len_buf).await?;
	let len = u32::from_be_bytes(len_buf) as usize;
	if len > 16 * 1024 * 1024 {
		bail!("message too large: {}", len);
	}
	let mut data = vec![0u8; len];
	stream.read_exact(&mut data).await?;
	Ok(serde_json::from_slice(&data)?)
}

// ---------------------------------------------------------------------------
// Key material (the secret odd integer)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Key {
	n: BigUint,
	bits: usize, // response length
}

impl Key {
	/// Generate a strong new key (default 256-bit odd integer).
	fn generate(bit_len: usize) -> Self {
		let mut rng = rand::thread_rng();
		let byte_len = (bit_len + 7) / 8;
		let mut bytes = vec![0u8; byte_len];
		loop {
			rng.fill_bytes(&mut bytes);
			// force odd and top bit set
			bytes[0] |= 0x80;
			*bytes.last_mut().unwrap() |= 1;
			let n = BigUint::from_bytes_be(&bytes);
			if !n.is_even() && n > BigUint::one() {
				return Key {
					n,
					bits: 256, // secure default
				};
			}
		}
	}

	/// Load from a key file (simple text format).
	fn load(path: &Path) -> Result<Self> {
		let content = fs::read_to_string(path)
			.with_context(|| format!("read key file {}", path.display()))?;
		let mut n_str = None;
		let mut bits = 256usize;

		for line in content.lines() {
			let line = line.trim();
			if line.is_empty() || line.starts_with('#') {
				continue;
			}
			if let Some(v) = line.strip_prefix("n=") {
				n_str = Some(v.trim());
			} else if let Some(v) = line.strip_prefix("bits=") {
				bits = v.trim().parse().context("bits=")?;
			}
		}

		let n = BigUint::from_str(n_str.context("key file missing n=")?)
			.context("invalid n in key file")?;
		if n.is_even() || n <= BigUint::one() {
			bail!("n must be odd and > 1");
		}
		Ok(Key { n, bits })
	}

	/// Save key with restrictive permissions (0600).
	fn save(&self, path: &Path) -> Result<()> {
		let mut f = OpenOptions::new()
			.write(true)
			.create(true)
			.truncate(true)
			.mode(0o600)
			.open(path)
			.with_context(|| format!("create key file {}", path.display()))?;

		writeln!(f, "# doors binary-reciprocal key")?;
		writeln!(f, "# keep this file secret")?;
		writeln!(f, "n={}", self.n)?;
		writeln!(f, "bits={}", self.bits)?;
		Ok(())
	}
}

// ---------------------------------------------------------------------------
// Binary reciprocal primitives
// ---------------------------------------------------------------------------

fn binary_reciprocal_bits(n: &BigUint, mut rem: BigUint, bits: usize) -> (String, BigUint) {
	debug_assert!(!n.is_even() && *n > BigUint::one());
	let mut s = String::with_capacity(bits);
	for _ in 0..bits {
		rem <<= 1;
		if &rem >= n {
			s.push('1');
			rem -= n;
		} else {
			s.push('0');
		}
	}
	(s, rem)
}

fn ct_eq(a: &str, b: &str) -> bool {
	if a.len() != b.len() {
		return false;
	}
	let mut diff = 0u8;
	for (x, y) in a.bytes().zip(b.bytes()) {
		diff |= x ^ y;
	}
	diff == 0
}

/// Challenge remainder is bound to both the nonce and the key itself.
fn challenge_remainder(key: &Key, nonce: &[u8]) -> BigUint {
	let mut hasher = Sha256::new();
	hasher.update(b"doors-chal-v1");
	hasher.update(nonce);
	hasher.update(key.n.to_bytes_be());
	let hash = hasher.finalize();
	let mut rem = BigUint::from_bytes_be(&hash);
	rem %= &key.n;
	if rem.is_zero() {
		rem = BigUint::one();
	}
	rem
}

// ---------------------------------------------------------------------------
// Door procedure (now takes a Key)
// ---------------------------------------------------------------------------

fn door_procedure(key: Option<&Key>, args: &str) -> String {
	let Some(key) = key else {
		// no key loaded → original unauthenticated behaviour
		return format!("Well, hello to you too! You said: {}", args);
	};

	// Challenge
	if let Some(nonce_hex) = args.strip_prefix("AUTH:CHAL:") {
		let nonce = match hex::decode(nonce_hex.trim()) {
			Ok(n) if n.len() >= 16 => n,
			_ => return "AUTH:ERR:bad nonce".into(),
		};
		let r0 = challenge_remainder(key, &nonce);
		return format!(
			"AUTH:CHAL:{}:{}",
			hex::encode(r0.to_bytes_be()),
			key.bits
		);
	}

	// Response
	if let Some(rest) = args.strip_prefix("AUTH:RESP:") {
		let parts: Vec<&str> = rest.splitn(2, ':').collect();
		if parts.len() != 2 {
			return "AUTH:ERR:malformed".into();
		}
		let nonce = match hex::decode(parts[0].trim()) {
			Ok(n) if n.len() >= 16 => n,
			_ => return "AUTH:ERR:bad nonce".into(),
		};
		let provided = parts[1].trim();

		let r0 = challenge_remainder(key, &nonce);
		let (expected, _) = binary_reciprocal_bits(&key.n, r0, key.bits);

		if ct_eq(provided, &expected) {
			return "AUTH:OK".into();
		} else {
			eprintln!("authentication failure from client");
			return "AUTH:ERR:bad key".into();
		}
	}

	// Unauthenticated message (still allowed)
	format!("Well, hello to you too! You said: {}", args)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

async fn handle_client(mut stream: UnixStream, key: Option<Key>) -> Result<()> {
	let req: DoorRequest = read_msg(&mut stream).await?;
	let reply = door_procedure(key.as_ref(), &req.data);
	let resp = DoorResponse {
		data: reply,
		error: None,
	};
	write_msg(&mut stream, &resp).await?;
	Ok(())
}

async fn run_server(path: PathBuf, key: Option<Key>) -> Result<()> {
	let _ = tokio::fs::remove_file(&path).await;
	let listener = UnixListener::bind(&path)
		.with_context(|| format!("bind {}", path.display()))?;

	if key.is_some() {
		println!("door attached at {} (authenticated)", path.display());
	} else {
		println!("door attached at {} (unauthenticated)", path.display());
	}

	loop {
		let (stream, _) = listener.accept().await?;
		let key = key.clone();
		tokio::spawn(async move {
			if let Err(e) = handle_client(stream, key).await {
				eprintln!("handler error: {:#}", e);
			}
		});
	}
}

// ---------------------------------------------------------------------------
// Client auth helper
// ---------------------------------------------------------------------------

async fn auth_handshake(path: &Path, key: &Key) -> Result<()> {
	let mut nonce = [0u8; 32];
	rand::thread_rng().fill_bytes(&mut nonce);
	let nonce_hex = hex::encode(nonce);

	let chal = door_call(path, &format!("AUTH:CHAL:{}", nonce_hex)).await?;
	let parts: Vec<&str> = chal.trim().splitn(3, ':').collect();
	if parts.len() < 3 || parts[0] != "AUTH" || parts[1] != "CHAL" {
		bail!("server did not send a challenge: {}", chal);
	}

	let r0_hex = parts[2].split(':').next().unwrap_or("");
	let r0 = BigUint::from_bytes_be(&hex::decode(r0_hex)?);
	let bits: usize = parts[2]
		.split(':')
		.nth(1)
		.and_then(|s| s.parse().ok())
		.unwrap_or(key.bits);

	let (bits_str, _) = binary_reciprocal_bits(&key.n, r0, bits);

	let resp = door_call(
		path,
		&format!("AUTH:RESP:{}:{}", nonce_hex, bits_str),
	)
	.await?;

	if resp.trim() == "AUTH:OK" {
		println!("authenticated successfully");
		Ok(())
	} else {
		bail!("authentication failed: {}", resp);
	}
}

// ---------------------------------------------------------------------------
// door_call (unchanged API)
// ---------------------------------------------------------------------------

async fn door_call(path: &Path, data: &str) -> Result<String> {
	let mut stream = UnixStream::connect(path)
		.await
		.with_context(|| format!("connect to {}", path.display()))?;

	let req = DoorRequest {
		data: data.to_string(),
	};
	write_msg(&mut stream, &req).await?;

	let resp: DoorResponse = read_msg(&mut stream).await?;
	if let Some(err) = resp.error {
		bail!("door error: {}", err);
	}
	Ok(resp.data)
}

// ---------------------------------------------------------------------------
// Concurrent runner (unchanged)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Event {
	Started { id: usize },
	Finished {
		id: usize,
		output: String,
		error: Option<String>,
	},
	Status {
		running: usize,
		done: usize,
		queued: usize,
	},
}

async fn run_clients(
	path: PathBuf,
	msg: String,
	count: usize,
	max_jobs: usize,
) -> Result<i32> {
	let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
	let sem = Arc::new(Semaphore::new(max_jobs));
	let done = Arc::new(AtomicUsize::new(0));
	let running = Arc::new(AtomicUsize::new(0));
	let failures = Arc::new(AtomicUsize::new(0));

	let mut handles = Vec::with_capacity(count);

	for id in 0..count {
		let sem = sem.clone();
		let tx = tx.clone();
		let path = path.clone();
		let msg = msg.clone();
		let done = done.clone();
		let running = running.clone();
		let failures = failures.clone();
		let total = count;

		handles.push(tokio::spawn(async move {
			let _permit = sem.acquire().await.expect("sem");
			running.fetch_add(1, Ordering::SeqCst);
			let _ = tx.send(Event::Started { id });
			let _ = tx.send(Event::Status {
				running: running.load(Ordering::SeqCst),
				done: done.load(Ordering::SeqCst),
				queued: total
					.saturating_sub(running.load(Ordering::SeqCst))
					.saturating_sub(done.load(Ordering::SeqCst)),
			});

			let result = door_call(&path, &msg).await;

			running.fetch_sub(1, Ordering::SeqCst);
			done.fetch_add(1, Ordering::SeqCst);

			match result {
				Ok(output) => {
					let _ = tx.send(Event::Finished {
						id,
						output,
						error: None,
					});
				}
				Err(e) => {
					failures.fetch_add(1, Ordering::SeqCst);
					let _ = tx.send(Event::Finished {
						id,
						output: String::new(),
						error: Some(e.to_string()),
					});
				}
			}

			let _ = tx.send(Event::Status {
				running: running.load(Ordering::SeqCst),
				done: done.load(Ordering::SeqCst),
				queued: total
					.saturating_sub(running.load(Ordering::SeqCst))
					.saturating_sub(done.load(Ordering::SeqCst)),
			});
		}));
	}

	drop(tx);

	while let Some(ev) = rx.recv().await {
		match ev {
			Event::Started { id } => println!("[{}] started", id),
			Event::Finished { id, output, error } => {
				if let Some(e) = error {
					println!("[{}] ERROR: {}", id, e);
				} else {
					println!("[{}] {}", id, output);
				}
			}
			Event::Status {
				running,
				done,
				queued,
			} => {
				eprintln!("status: running={} done={} queued={}", running, done, queued);
			}
		}
	}

	for h in handles {
		let _ = h.await;
	}

	Ok(if failures.load(Ordering::SeqCst) == 0 {
		0
	} else {
		1
	})
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
	name = "doors",
	version = "0.3.0",
	about = "Solaris Doors IPC with easy binary-reciprocal key/lock"
)]
struct Args {
	#[command(subcommand)]
	cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
	/// Generate a new strong key file
	Keygen {
		/// Output path for the key
		#[arg(long, default_value = "doors.key")]
		out: PathBuf,

		/// Bit length of the secret (default 256)
		#[arg(long, default_value_t = 256)]
		bits: usize,
	},

	/// Start the door server
	Server {
		#[arg(long, default_value = "/tmp/hello.door")]
		path: PathBuf,

		/// Path to key file (or set DOORS_KEY env)
		#[arg(long)]
		key: Option<PathBuf>,
	},

	/// Call the door
	Client {
		#[arg(long, default_value = "/tmp/hello.door")]
		path: PathBuf,

		#[arg(long, default_value = "Hello, World!")]
		msg: String,

		#[arg(short = 'n', long, default_value_t = 1)]
		count: usize,

		#[arg(short = 'm', long, default_value_t = 8)]
		max_jobs: usize,

		/// Authenticate first
		#[arg(long)]
		auth: bool,

		/// Path to key file (required with --auth, or set DOORS_KEY)
		#[arg(long)]
		key: Option<PathBuf>,
	},
}

fn load_key(explicit: Option<PathBuf>) -> Result<Option<Key>> {
	let path = explicit
		.or_else(|| std::env::var_os("DOORS_KEY").map(PathBuf::from));
	match path {
		Some(p) => Ok(Some(Key::load(&p)?)),
		None => Ok(None),
	}
}

#[tokio::main]
async fn main() -> Result<()> {
	let args = Args::parse();

	match args.cmd {
		Cmd::Keygen { out, bits } => {
			let key = Key::generate(bits);
			key.save(&out)?;
			println!("wrote key → {}", out.display());
			println!("keep this file secret (mode 0600)");
		}

		Cmd::Server { path, key } => {
			let key = load_key(key)?;
			run_server(path, key).await?;
		}

		Cmd::Client {
			path,
			msg,
			count,
			max_jobs,
			auth,
			key,
		} => {
			if auth {
				let key = load_key(key)?.context("--auth requires a key (--key or DOORS_KEY)")?;
				auth_handshake(&path, &key).await?;
				// after successful auth we can still send a normal message
				if !msg.is_empty() && msg != "Hello, World!" {
					let reply = door_call(&path, &msg).await?;
					println!("{}", reply);
				}
			} else {
				let code = run_clients(path, msg, count, max_jobs).await?;
				std::process::exit(code);
			}
		}
	}
	Ok(())
}