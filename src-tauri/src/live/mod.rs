// SPDX-License-Identifier: Apache-2.0
//! Studio Live: the session server that lets teammates work on one file together (spec docs/STUDIO-LIVE-SPEC.md,
//! messages docs/LIVE-PROTOCOL.md, both in the site repo; plan track LV in docs/0.5-PLAN.md).
//!
//! - `msg`: the MessagePack envelope.
//! - `session`: the pure core (who is in, the order, property vs structural ops, holds, locks, validate, catch-up).
//! - `net`: WebSocket transport on its own thread, frame keys per connection; in-process clients for the host window
//!   and the replica.
//! - `crypto` (session live-relay): the PAKE handshake and frame encryption (docs/LIVE-CRYPTO.md).
//! - `http`: the same port serves the browser edition (LV.12).
//! - `relay`: the host's side of the relay for invite links (docs/LIVE-RELAY.md, session live-relay's service).
//! - `ws`: outgoing WebSockets (`ws_connect`: the OS's TLS, rustls on Android), used by `relay` and the shell's glue.
//!
//! Owner: session live-core, except `crypto.rs` (session live-relay: handshake and frame encryption).
//! No Tauri types in here: the shell's glue (windows, commands) lives with the code that opens the session, and
//! `tools/live-test/` in the site repo builds this module on its own for the tests.

pub mod crypto;
pub mod http;
pub mod msg;
pub mod net;
pub mod relay;
pub mod session;
pub mod ws;

#[allow(unused_imports)]
pub use net::{start, start_from, LiveHandle, LocalClient, Serve, Wire};
#[allow(unused_imports)]
pub use relay::RelayHandle;
#[allow(unused_imports)]
pub use session::{Config, Role};
