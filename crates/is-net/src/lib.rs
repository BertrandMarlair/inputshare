#![forbid(unsafe_code)]

//! InputShare networking.
//!
//! Everything needed to turn a pile of separately configured machines into one
//! workspace that finds itself again after a reboot:
//!
//! - [`discovery`] — continuous UDP multicast announce and listen, so machines
//!   that boot at different times still find each other (requirement 5).
//! - [`crypto`] — an authenticated, encrypted channel. The link carries
//!   keystrokes, so this is not optional.
//! - [`session`] — one peer connection: handshake, authorisation against the
//!   paired record, document exchange, then live updates.
//! - [`service`] — the actor that ties those together, reconnects automatically
//!   and costs nothing while a machine is switched off (requirements 6 and 7).
//!
//! The crate never touches disk. It reaches the workspace through the
//! [`Workspace`] trait, so persistence stays in one place.

pub mod crypto;
pub mod discovery;
pub mod service;
pub mod session;
pub mod wire;

pub use service::{Config, Event, Service, Workspace};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network io: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not encode or decode a message: {0}")]
    Json(#[from] serde_json::Error),

    #[error("handshake refused: {0}")]
    Handshake(&'static str),

    #[error("encryption failure: {0}")]
    Crypto(&'static str),

    #[error("workspace: {0}")]
    Core(#[from] is_core::Error),

    #[error("frame of {0} bytes exceeds the limit")]
    FrameTooLarge(usize),

    #[error("timed out")]
    Timeout,

    #[error("service has stopped")]
    Closed,
}
