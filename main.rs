//! doors – Solaris/illumos Doors IPC re-created for Linux & macOS
//!
//! Single-file recreation inspired by:
//!   https://github.com/robertdfrench/revolving-doors
//! and structured like:
//!   https://github.com/alexanderdfox/AIP
//!
//! Core ideas preserved:
//! - door_create  → register a handler that lives in a server process
//! - fattach      → bind the door to a path (Unix domain socket)
//! - door_call    → client RPC that looks like a local function call
//! - automatic handler-thread (task) management
//! - data in / data out (no descriptors in this minimal version)
//!
//! Usage:
//!   # start server (keeps running)
//!   ./doors server --path /tmp/hello.door
//!
//!   # client call
//!   ./doors client --path /tmp/hello.door --msg "Hello, World!"
//!
//!   # concurrent clients (AIP-style max-jobs)
//!   ./doors client --path /tmp/hello.door --msg "Hello" -m 8 -n 20

use anyhow::{bail, Context, Result};
use bytes::{BufMut, BytesMut};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Wire protocol (simple length-prefixed JSON, mirrors door_arg_t data_ptr/size)
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
// Server side  (door_create + fattach + automatic handler tasks)
// ---------------------------------------------------------------------------

/// The “door procedure” – equivalent to the `answer` function in revolving-doors 80_hello_world.
fn door_procedure(cookie: Option<&str>, args: &str) -> String {
	// cookie can be used for per-door state (like the cookie argument in door_create)
	let _ = cookie;
	format!("Well, hello to you too! You said: {}", args)
}

async fn handle_client(mut stream: UnixStream, cookie: Option<String>) -> Result<()> {
	let req: DoorRequest = read_msg(&mut stream).await?;
	let reply = door_procedure(cookie.as_deref(), &req.data);
	let resp = DoorResponse {
		data: reply,
		error: None,
	};
	write_msg(&mut stream, &resp).await?;
	Ok(())
}

async fn run_server(path: PathBuf, cookie: Option<String>) -> Result<()> {
	// Clean up previous socket (fattach semantics)
	let _ = tokio::fs::remove_file(&path).await;

	let listener = UnixListener::bind(&path)
		.with_context(|| format!("bind {}", path.display()))?;

	println!("door attached at {} (sleeping forever, Ctrl-C to stop)", path.display());

	// Automatic handler-task management (mirrors door thread pool)
	loop {
		let (stream, _) = listener.accept().await?;
		let cookie = cookie.clone();
		tokio::spawn(async move {
			if let Err(e) = handle_client(stream, cookie).await {
				eprintln!("handler error: {:#}", e);
			}
		});
	}
}

// ---------------------------------------------------------------------------
// Client side  (door_call)
// ---------------------------------------------------------------------------

async fn door_call(path: &PathBuf, data: &str) -> Result<String> {
	let mut stream = UnixStream::connect(path)
		.await
		.with_context(|| format!("connect to door {}", path.display()))?;

	let req = DoorRequest {
		data: data.to_string(),
	};
	write_msg(&mut stream, &req).await?;

	let resp: DoorResponse = read_msg(&mut stream).await?;
	if let Some(err) = resp.error {
		bail!("door returned error: {}", err);
	}
	Ok(resp.data)
}

// ---------------------------------------------------------------------------
// AIP-style concurrent client runner (max-jobs, event stream, aggregate status)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Event {
	Started { id: usize },
	Finished { id: usize, output: String, error: Option<String> },
	Status { running: usize, done: usize, queued: usize },
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

	let mut handles: Vec<JoinHandle<()>> = Vec::with_capacity(count);

	for id in 0..count {
		let sem = sem.clone();
		let tx = tx.clone();
		let path = path.clone();
		let msg = msg.clone();
		let done = done.clone();
		let running = running.clone();
		let failures = failures.clone();
		let total = count;

		let h = tokio::spawn(async move {
			let _permit = sem.acquire().await.expect("sem closed");
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
		});
		handles.push(h);
	}

	drop(tx); // close channel when all tasks finish

	// Print events (LINE-mode style)
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
			Event::Status { running, done, queued } => {
				eprintln!("status: running={} done={} queued={}", running, done, queued);
			}
		}
	}

	for h in handles {
		let _ = h.await;
	}

	let fails = failures.load(Ordering::SeqCst);
	Ok(if fails == 0 { 0 } else { 1 })
}

// ---------------------------------------------------------------------------
// CLI  (same shape as AIP)
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
	name = "doors",
	version = "0.1.0",
	about = "Solaris Doors IPC re-created for Linux & macOS (single-file, AIP-style)"
)]
struct Args {
	#[command(subcommand)]
	cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
	/// Create a door and attach it to a path (server)
	Server {
		/// Path that will name the door (like fattach)
		#[arg(long, default_value = "/tmp/hello.door")]
		path: PathBuf,

		/// Optional cookie (opaque state passed to the door procedure)
		#[arg(long)]
		cookie: Option<String>,
	},

	/// Call the door (client)
	Client {
		#[arg(long, default_value = "/tmp/hello.door")]
		path: PathBuf,

		/// Data to send (door_arg_t.data_ptr)
		#[arg(long, default_value = "Hello, World!")]
		msg: String,

		/// Number of concurrent calls (AIP-style)
		#[arg(short = 'n', long, default_value_t = 1)]
		count: usize,

		/// Max concurrent jobs
		#[arg(short = 'm', long, default_value_t = 8)]
		max_jobs: usize,
	},
}

#[tokio::main]
async fn main() -> Result<()> {
	let args = Args::parse();

	match args.cmd {
		Cmd::Server { path, cookie } => {
			run_server(path, cookie).await?;
		}
		Cmd::Client {
			path,
			msg,
			count,
			max_jobs,
		} => {
			let code = run_clients(path, msg, count, max_jobs).await?;
			std::process::exit(code);
		}
	}
	Ok(())
}