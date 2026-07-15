//! Low-level USB transport for the scope.
//!
//! [`Transport`] owns the libusb device handle and implements the connection recipe and
//! the reader-thread-before-write transaction dance the device requires. Higher layers
//! ([`crate::Scope`]) talk to the scope exclusively through [`Transport::transact`] and
//! [`Transport::recv`].

mod transport;

pub use transport::Transport;
