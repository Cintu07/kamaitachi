//! Networking. `conn` and `http` are plain sockets, TLS and an HTTP/1.1 parser, and
//! build anywhere; the browser's engine fetches pages with them. `thread` is the
//! terminal client's net thread, which waits on Windows socket events, so it only
//! builds there.

pub mod conn;
pub mod cookies;
pub mod http;
pub mod sse;
#[cfg(windows)]
mod thread;

pub use conn::Host;
#[cfg(windows)]
pub use thread::*;
