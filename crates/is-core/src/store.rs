//! Local persistence (requirements 2 and 12).
//!
//! Three files, all JSON, all in one directory:
//!
//! - `identity.json` — the permanent machine ID and secret key. Written once.
//! - `workspace.json` — the synchronized workspace document.
//! - `peer-hints.json` — last-known addresses, a pure cache.
//! - `local-prefs.json` — choices that belong to this computer alone.
//!
//! The hints file is separate on purpose. Addresses are runtime metadata that
//! change on every DHCP lease, so they must not ride along inside the
//! synchronized document and cause a configuration revision on every reconnect.
//! Losing the hints file costs nothing; discovery finds the peers again.
//!
//! Writes go through a temp file and a rename, and the previous copy is kept as
//! `.bak`. A machine that loses power mid-write must come back up with the old
//! workspace, not with no workspace — a truncated file that fails to parse would
//! otherwise look exactly like a first launch, which is the reset that the
//! critical requirement rules out.

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{paths, Error, Identity, MachineId, Result, WorkspaceDoc};

pub const IDENTITY_FILE: &str = "identity.json";
pub const WORKSPACE_FILE: &str = "workspace.json";
pub const HINTS_FILE: &str = "peer-hints.json";
pub const PREFS_FILE: &str = "local-prefs.json";

/// Where a peer was last reachable. A hint, never a source of truth: discovery
/// works from scratch when every address has changed (requirement 5).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerHint {
    pub addrs: Vec<SocketAddr>,
    pub hostname: Option<String>,
    pub last_seen_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeerHints {
    pub peers: BTreeMap<MachineId, PeerHint>,
}

/// Choices that belong to this computer and travel with nothing.
///
/// Deliberately *not* part of the workspace document. The document is
/// synchronized, so putting "share this keyboard and mouse" in it would mean
/// that switching sharing on here switches it on everywhere — which is the
/// opposite of what the switch says. Whether a given computer hands over its own
/// keyboard is that computer's business.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalPrefs {
    /// Whether keyboard and mouse sharing was on when this machine was last
    /// used, so a restart resumes it instead of quietly dropping it.
    pub sharing: bool,
}

/// Everything needed to restore a machine into its workspace (requirement 14).
///
/// Contains the machine's secret key, so a backup is as sensitive as the config
/// directory itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    pub identity: Identity,
    pub workspace: Option<WorkspaceDoc>,
}

/// The config directory, and the only thing that reads or writes it.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Opens the platform config directory, creating it if needed.
    pub fn open() -> Result<Self> {
        Self::at(paths::config_dir()?)
    }

    pub fn at(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads this machine's identity, minting one on first launch.
    ///
    /// An existing file is never overwritten. Regenerating the identity would
    /// change the machine ID, and every peer would see a stranger instead of the
    /// machine they paired with.
    pub fn load_or_create_identity(&self) -> Result<Identity> {
        if let Some(identity) = self.read_with_fallback::<Identity>(IDENTITY_FILE)? {
            return Ok(identity);
        }
        let identity = Identity::generate();
        self.write_atomic(IDENTITY_FILE, &identity)?;
        restrict_permissions(&self.dir.join(IDENTITY_FILE));
        Ok(identity)
    }

    /// The stored workspace, or `None` if this machine has not joined one.
    pub fn load_workspace(&self) -> Result<Option<WorkspaceDoc>> {
        let doc: Option<WorkspaceDoc> = self.read_with_fallback(WORKSPACE_FILE)?;
        if let Some(doc) = &doc {
            if doc.schema_version > WorkspaceDoc::SCHEMA_VERSION {
                // Refuse rather than parse a newer layout and silently drop the
                // fields this build does not know about, which would then
                // propagate the loss to every peer on the next merge.
                return Err(Error::SchemaVersion {
                    found: doc.schema_version,
                    supported: WorkspaceDoc::SCHEMA_VERSION,
                });
            }
        }
        Ok(doc)
    }

    pub fn save_workspace(&self, doc: &WorkspaceDoc) -> Result<()> {
        self.write_atomic(WORKSPACE_FILE, doc)
    }

    /// Forgets the workspace, keeping this machine's identity.
    ///
    /// Used when a machine gives up its own workspace to join somebody else's —
    /// the one case where losing the local configuration is the point. The
    /// backup copy goes too, or the next start would quietly restore what the
    /// user just chose to abandon.
    pub fn clear_workspace(&self) -> Result<()> {
        for name in [
            WORKSPACE_FILE.to_string(),
            format!("{WORKSPACE_FILE}.bak"),
            format!("{WORKSPACE_FILE}.tmp"),
        ] {
            let path = self.dir.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path, source }),
            }
        }
        Ok(())
    }

    pub fn load_hints(&self) -> Result<PeerHints> {
        Ok(self.read_with_fallback(HINTS_FILE)?.unwrap_or_default())
    }

    pub fn save_hints(&self, hints: &PeerHints) -> Result<()> {
        self.write_atomic(HINTS_FILE, hints)
    }

    /// This computer's own choices. A missing or unreadable file is not an
    /// error: preferences are a convenience, and refusing to start because one
    /// could not be read would be worse than starting with the defaults.
    pub fn load_prefs(&self) -> LocalPrefs {
        match self.read_with_fallback::<LocalPrefs>(PREFS_FILE) {
            Ok(Some(prefs)) => prefs,
            Ok(None) => LocalPrefs::default(),
            Err(error) => {
                warn!(%error, "local preferences unreadable, using the defaults");
                LocalPrefs::default()
            }
        }
    }

    pub fn save_prefs(&self, prefs: &LocalPrefs) -> Result<()> {
        self.write_atomic(PREFS_FILE, prefs)
    }

    /// Writes a portable copy of identity and workspace to `path`.
    pub fn export_backup(&self, path: &Path) -> Result<()> {
        let backup = Backup {
            identity: self.load_or_create_identity()?,
            workspace: self.load_workspace()?,
        };
        let bytes = serde_json::to_vec_pretty(&backup)?;
        fs::write(path, bytes).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        restrict_permissions(path);
        Ok(())
    }

    /// Restores a backup over this machine's configuration.
    ///
    /// This adopts the backup's machine ID and key, so the machine resumes the
    /// identity it had before a reinstall. Destructive by design: the caller is
    /// expected to have asked the user first.
    pub fn import_backup(&self, path: &Path) -> Result<Identity> {
        let backup: Backup = read_json(path)?.ok_or_else(|| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(ErrorKind::NotFound, "backup file not found"),
        })?;
        self.write_atomic(IDENTITY_FILE, &backup.identity)?;
        restrict_permissions(&self.dir.join(IDENTITY_FILE));
        if let Some(doc) = &backup.workspace {
            self.write_atomic(WORKSPACE_FILE, doc)?;
        }
        Ok(backup.identity)
    }

    /// Reads `name`, falling back to the `.bak` copy if the primary file is
    /// unreadable.
    fn read_with_fallback<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.dir.join(name);
        match read_json(&path) {
            Ok(value) => Ok(value),
            Err(primary) => {
                let backup = self.dir.join(format!("{name}.bak"));
                match read_json::<T>(&backup) {
                    Ok(Some(value)) => {
                        warn!(
                            file = %path.display(),
                            error = %primary,
                            "config file unreadable, recovered from .bak"
                        );
                        Ok(Some(value))
                    }
                    _ => Err(primary),
                }
            }
        }
    }

    /// Temp file, fsync, keep a `.bak`, then rename into place.
    fn write_atomic<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.tmp"));
        let bytes = serde_json::to_vec_pretty(value)?;

        let mut file = fs::File::create(&tmp).map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
        file.write_all(&bytes).map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
        drop(file);

        if path.exists() {
            let backup = self.dir.join(format!("{name}.bak"));
            fs::copy(&path, &backup).map_err(|source| Error::Io {
                path: backup,
                source,
            })?;
        }

        // Replaces the destination on both Windows and Unix.
        fs::rename(&tmp, &path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            }),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Narrows a secret-bearing file to the owner where the platform supports it.
/// Best-effort: failing to tighten permissions is not a reason to refuse to run.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        warn!(file = %path.display(), %error, "could not restrict file permissions");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}
