//! A batteries-included Unix-socket daemon harness. Includes goodies such as
//! double-fork daemonization with a readiness pipe, a signal self-pipe,
//! `SO_PEERCRED` authorization, stale-socket reclaim, a length-limited frame
//! reader, a concurrency limiter, a locked pidfile, and an accept-loop driver
//! that wires them together.
//!
//! `knell` packages the lifecycle plumbing every Unix-socket daemon
//! reimplements: becoming a daemon cleanly, telling the launching shell whether
//! it came up, shutting down on a signal without racing its accept loop,
//! binding a socket nobody else can read, and refusing connections from the
//! wrong user.
//!
//! # Write a daemon in 20 lines or less
//!
//! The parent process blocks inside [`Daemonize::daemonize`] until the daemon
//! calls [`Readiness::ready`] (exit 0) or [`Readiness::failed`] (exit 1), so
//! the launching shell learns whether startup succeeded. The daemon then serves
//! connections through [`Server`], which polls the listener and the
//! [`SignalPipe`] together and caps concurrency with a [`ConnectionLimiter`].
//!
//! [`Daemonize::daemonize`]: daemonize::Daemonize::daemonize
//! [`Readiness::ready`]: daemonize::Readiness::ready
//! [`Readiness::failed`]: daemonize::Readiness::failed
//! [`Server`]: server::Server
//! [`SignalPipe`]: signals::SignalPipe
//! [`ConnectionLimiter`]: connection_limit::ConnectionLimiter
//!
//! ```no_run
//! use std::io::Write as _;
//!
//! use knell::{
//!   daemonize::Daemonize,
//!   pidfile::PidFile,
//!   server::Server,
//!   signals::SignalPipe,
//!   socket::{
//!     bind_unix,
//!     require_uid,
//!   },
//! };
//!
//! fn main() -> knell::error::Result<()> {
//!   let mut readiness = Daemonize::new().daemonize()?;
//!   let _pidfile = PidFile::create("/run/user/1000/example.pid")?;
//!   let listener = bind_unix("/run/user/1000/example.sock", 0o600)?;
//!   let signals = SignalPipe::install()?;
//!   let me = unsafe { libc::getuid() };
//!   readiness.ready_with("listening");
//!
//!   Server::new(listener, signals, 64).run(move |mut stream| {
//!     if require_uid(&stream, me).is_ok() {
//!       let _ = stream.write_all(b"hello\n");
//!     }
//!   })
//! }
//! ```

#![warn(missing_docs)]

pub mod connection_limit;
pub mod daemonize;
pub mod error;
pub mod framed;
pub mod pidfile;
pub mod server;
pub mod signals;
pub mod socket;
