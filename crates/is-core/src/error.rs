//! Error type for the core domain.

use std::path::PathBuf;

use crate::WorkspaceId;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    PlainIo(#[from] std::io::Error),

    #[error("could not parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("no config directory available on this platform")]
    NoConfigDir,

    #[error("malformed hex: {0}")]
    Hex(String),

    #[error("invalid public key: {0}")]
    BadKey(String),

    #[error("expected {expected} bytes, got {got}")]
    KeyLength { expected: usize, got: usize },

    #[error("update belongs to workspace {incoming}, this machine is in workspace {local}")]
    WorkspaceMismatch {
        incoming: WorkspaceId,
        local: WorkspaceId,
    },

    #[error("workspace file has schema version {found}, this build supports up to {supported}")]
    SchemaVersion { found: u32, supported: u32 },
}
