//! What goes over the wire, and how it is framed.

use is_core::{InputEvent, MachineId, PubKey, WorkspaceDoc, WorkspaceId, WorkspaceUpdate};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::crypto::PROTOCOL_VERSION;
use crate::{Error, Result};

/// A whole workspace document has to fit in one frame, and documents grow with
/// the number of machines and displays. Generous, but bounded: an unbounded
/// length prefix is a way to ask a peer to allocate all of memory.
const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Sent over UDP so machines can find each other (requirement 5).
///
/// Carries no trust whatsoever. Anyone on the network can send one claiming any
/// machine ID; all it does is prompt a connection attempt, and the handshake on
/// that connection is what actually decides who the peer is. It is a hint about
/// *where* to look, never about *who* is there.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Announcement {
    pub protocol: u16,
    pub machine_id: MachineId,
    pub identity_key: PubKey,
    pub display_name: String,
    /// `None` for a machine that has not joined a workspace yet — it still
    /// announces, so it can show up as a pairing candidate.
    pub workspace_id: Option<WorkspaceId>,
    pub transport_port: u16,
    /// Set on an announcement sent straight to a typed-in address, asking the
    /// receiver to answer once so both machines learn about each other from a
    /// single entry.
    ///
    /// Defaults to false so an announcement from an older build still parses.
    #[serde(default)]
    pub probe: bool,
    /// Machines this one has admitted but is not connected to.
    ///
    /// Pairing needs both sides to agree, and without this the second side has
    /// no idea it was asked: the person clicks Pair on one machine, nothing
    /// happens, and there is nothing on screen to explain why. Carrying the
    /// invitation turns that into "this computer invited you".
    ///
    /// Only ids of machines not currently connected, so the list stays short and
    /// stops being advertised once it has served its purpose.
    #[serde(default)]
    pub invitations: Vec<MachineId>,
}

impl Announcement {
    pub fn is_current(&self) -> bool {
        self.protocol == PROTOCOL_VERSION
    }
}

/// Everything after the handshake, encrypted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// First message on a fresh session.
    Identify {
        display_name: String,
        workspace_id: Option<WorkspaceId>,
        /// How many machines the sender's configuration holds.
        ///
        /// Lets two machines that each set up on their own decide, without
        /// asking a person, which configuration survives: the bigger one, and
        /// the lower id when they are the same size. Both sides compute the same
        /// answer from the same two numbers.
        #[serde(default)]
        machines: u32,
    },
    /// The sender's whole document. Both sides send one on connect and merge
    /// what they receive, because neither can assume it holds the newer state
    /// (requirement 9).
    DocFull {
        doc: Box<WorkspaceDoc>,
    },
    /// A single change, while both machines are connected (requirement 10).
    Update {
        update: Box<WorkspaceUpdate>,
    },
    /// Keyboard and mouse events captured on the sender, to be replayed here.
    ///
    /// Batched because motion arrives in bursts, and one frame per mouse sample
    /// would spend more time on framing and encryption than on the event itself.
    Input {
        events: Vec<InputEvent>,
    },
    /// "The cursor is on this machine now."
    ///
    /// Broadcast whenever ownership moves. Exactly one machine owns the pointer
    /// at a time and everyone has to agree which — without this each machine
    /// decides for itself, they drift into both believing they have it, and the
    /// result is a pointer that ping-pongs and a keyboard that goes dead for no
    /// visible reason.
    CursorOwner {
        machine: MachineId,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    Goodbye {
        reason: String,
    },
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_FRAME {
        return Err(Error::FrameTooLarge(bytes.len()));
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(Error::FrameTooLarge(length));
    }
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}
