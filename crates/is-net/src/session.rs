//! One connection to one peer, from handshake to teardown.
//!
//! The order here is the security-relevant part:
//!
//! 1. Handshake — proves the peer holds the private key for the identity key it
//!    presents. Encrypts everything after it.
//! 2. **Authorisation** — checks that key against the record we stored when the
//!    machines were paired. A peer that proves an identity we never paired with
//!    is refused here, before it can send us a single workspace message.
//! 3. Only then: identify, exchange documents, stream updates.
//!
//! Step 2 is the one that is easy to leave out and expensive to leave out. The
//! handshake alone proves *who* someone is, not that they belong here; without
//! the check, any machine on the network could hand us a document and rewrite
//! the workspace.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use is_core::{Identity, MachineId, PubKey, WorkspaceId};
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::debug;

use crate::crypto::{self, Hello, Opener, Sealer};
use crate::service::{Internal, Workspace};
use crate::wire::{self, Message};
use crate::{Error, Result};

/// How often we prove we are still here, and how long we wait before deciding a
/// peer is not. Silence is the only reliable signal: a machine that was
/// suspended or unplugged never gets to send a goodbye.
const HEARTBEAT: Duration = Duration::from_secs(5);
const SILENCE_LIMIT: Duration = Duration::from_secs(15);

pub type Outbound = mpsc::Sender<Message>;

/// What a session tells the actor about itself.
pub enum SessionOutcome {
    Established {
        machine_id: MachineId,
        display_name: String,
        outbound: Outbound,
    },
    Closed {
        machine_id: MachineId,
        reason: String,
    },
    Rejected {
        machine_id: MachineId,
        identity_key: PubKey,
        addr: SocketAddr,
    },
    /// Both machines are in a workspace, and they are not the same one.
    ///
    /// This cannot be resolved by retrying: two workspaces have different
    /// identifiers and the merge refuses to mix them, by design. Reported as its
    /// own outcome so it can be explained rather than looping forever as an
    /// unexplained failure to connect.
    DifferentWorkspaces {
        machine_id: MachineId,
        display_name: String,
        identity_key: PubKey,
        their_workspace: WorkspaceId,
        their_size: u32,
    },
    WorkspaceChanged,
}

pub(crate) fn spawn_dial(
    identity: Identity,
    workspace: Arc<dyn Workspace>,
    expect: MachineId,
    addr: SocketAddr,
    inbox: mpsc::Sender<Internal>,
) {
    tokio::spawn(async move {
        let result = async {
            let stream = crate::service::dial(addr).await?;
            run(
                identity,
                workspace,
                stream,
                addr,
                true,
                Some(expect),
                &inbox,
            )
            .await
        }
        .await;

        if let Err(error) = result {
            debug!(machine = %expect, %error, "dial failed");
            let _ = inbox
                .send(Internal::Session(SessionOutcome::Closed {
                    machine_id: expect,
                    reason: error.to_string(),
                }))
                .await;
        }
    });
}

pub(crate) fn spawn_accepted(
    identity: Identity,
    workspace: Arc<dyn Workspace>,
    stream: TcpStream,
    addr: SocketAddr,
    inbox: mpsc::Sender<Internal>,
) {
    tokio::spawn(async move {
        if let Err(error) = run(identity, workspace, stream, addr, false, None, &inbox).await {
            debug!(%addr, %error, "inbound connection ended");
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run(
    identity: Identity,
    workspace: Arc<dyn Workspace>,
    stream: TcpStream,
    addr: SocketAddr,
    dialled: bool,
    expect: Option<MachineId>,
    inbox: &mpsc::Sender<Internal>,
) -> Result<()> {
    // Nagle would add latency to the small frames this protocol is made of, and
    // cursor movement is the thing a user notices first.
    let _ = stream.set_nodelay(true);
    let (mut reader, mut writer) = stream.into_split();

    // --- 1. handshake -------------------------------------------------------
    let initiation = crypto::begin(&identity);
    wire::write_frame(&mut writer, &serde_json::to_vec(&initiation.hello)?).await?;
    let peer_hello: Hello = serde_json::from_slice(
        &tokio::time::timeout(Duration::from_secs(10), wire::read_frame(&mut reader))
            .await
            .map_err(|_| Error::Timeout)??,
    )?;
    let session = initiation.complete(&peer_hello, dialled)?;
    let machine_id = session.peer_machine;
    let identity_key = session.peer_key;

    if let Some(expected) = expect {
        if expected != machine_id {
            return Err(Error::Handshake("connected to a different machine"));
        }
    }

    // --- 2. authorisation ---------------------------------------------------
    if !workspace.authorize(machine_id, &identity_key) {
        let _ = inbox
            .send(Internal::Session(SessionOutcome::Rejected {
                machine_id,
                identity_key,
                addr,
            }))
            .await;
        let _ = writer.shutdown().await;
        return Ok(());
    }

    // --- 3. identify --------------------------------------------------------
    let (mut sealer, mut opener) = session.split();
    let ours = workspace.snapshot();
    let hello = Message::Identify {
        display_name: workspace.display_name(),
        workspace_id: ours.as_ref().map(|doc| doc.workspace_id),
        machines: ours
            .as_ref()
            .map(|doc| doc.machines_present().count() as u32)
            .unwrap_or(0),
    };
    send(&mut writer, &mut sealer, &hello).await?;

    let (display_name, their_workspace, their_size) =
        match recv(&mut reader, &mut opener, SILENCE_LIMIT).await? {
            Message::Identify {
                display_name,
                workspace_id,
                machines,
            } => (display_name, workspace_id, machines),
            _ => return Err(Error::Handshake("peer did not identify itself")),
        };

    // Checked here, before either side sends a document. Letting it reach the
    // merge produces a rejected document and a dropped connection every few
    // seconds, with nothing to say why.
    if let (Some(ours), Some(theirs)) = (ours.as_ref().map(|d| d.workspace_id), their_workspace) {
        if ours != theirs {
            let _ = inbox
                .send(Internal::Session(SessionOutcome::DifferentWorkspaces {
                    machine_id,
                    display_name,
                    identity_key,
                    their_workspace: theirs,
                    their_size,
                }))
                .await;
            let _ = send(
                &mut writer,
                &mut sealer,
                &Message::Goodbye {
                    reason: "our configurations are separate; one of us will adopt the other"
                        .into(),
                },
            )
            .await;
            return Ok(());
        }
    }

    let (outbound, mut queue) = mpsc::channel::<Message>(128);
    inbox
        .send(Internal::Session(SessionOutcome::Established {
            machine_id,
            display_name,
            outbound,
        }))
        .await
        .map_err(|_| Error::Closed)?;

    // --- 4. sync ------------------------------------------------------------
    // Both sides send their whole document and merge what arrives. Neither is
    // treated as authoritative, so a machine that has been off for a week
    // cannot overwrite what happened while it was away (requirement 9).
    if let Some(doc) = workspace.snapshot() {
        send(
            &mut writer,
            &mut sealer,
            &Message::DocFull { doc: Box::new(doc) },
        )
        .await?;
    }

    let writer_task = tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut beat = 0u64;
        loop {
            tokio::select! {
                outgoing = queue.recv() => match outgoing {
                    Some(message) => {
                        if send(&mut writer, &mut sealer, &message).await.is_err() {
                            return;
                        }
                    }
                    None => {
                        let _ = writer.shutdown().await;
                        return;
                    }
                },
                _ = heartbeat.tick() => {
                    beat += 1;
                    if send(&mut writer, &mut sealer, &Message::Ping { nonce: beat })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });

    let reason = read_loop(machine_id, &mut reader, &mut opener, &workspace, inbox).await;
    writer_task.abort();

    let _ = inbox
        .send(Internal::Session(SessionOutcome::Closed {
            machine_id,
            reason,
        }))
        .await;
    Ok(())
}

/// Reads until the peer stops making sense or stops talking. Returns why.
async fn read_loop(
    peer: MachineId,
    reader: &mut OwnedReadHalf,
    opener: &mut Opener,
    workspace: &Arc<dyn Workspace>,
    inbox: &mpsc::Sender<Internal>,
) -> String {
    loop {
        let message = match recv(reader, opener, SILENCE_LIMIT).await {
            Ok(message) => message,
            Err(Error::Timeout) => return "peer went quiet".into(),
            Err(error) => return error.to_string(),
        };

        match message {
            Message::DocFull { doc } => match workspace.merge(&doc) {
                Ok(true) => {
                    let _ = inbox
                        .send(Internal::Session(SessionOutcome::WorkspaceChanged))
                        .await;
                }
                Ok(false) => {}
                Err(error) => return format!("incompatible workspace: {error}"),
            },
            Message::Update { update } => match workspace.apply(*update) {
                Ok(true) => {
                    let _ = inbox
                        .send(Internal::Session(SessionOutcome::WorkspaceChanged))
                        .await;
                }
                Ok(false) => {}
                Err(error) => return format!("rejected update: {error}"),
            },
            // Ping and Pong carry no information beyond arriving at all, which
            // is the whole point: they keep the silence timer from firing.
            Message::Input { events } => workspace.on_input(peer, events),
            Message::CursorOwner { machine } => workspace.on_cursor_owner(peer, machine),
            Message::Ping { .. } | Message::Pong { .. } => {}
            Message::Identify { .. } => {}
            Message::Goodbye { reason } => return reason,
        }
    }
}

async fn send(writer: &mut OwnedWriteHalf, sealer: &mut Sealer, message: &Message) -> Result<()> {
    let plaintext = serde_json::to_vec(message)?;
    let sealed = sealer.seal(&plaintext)?;
    wire::write_frame(writer, &sealed).await
}

async fn recv(
    reader: &mut OwnedReadHalf,
    opener: &mut Opener,
    within: Duration,
) -> Result<Message> {
    let sealed = tokio::time::timeout(within, wire::read_frame(reader))
        .await
        .map_err(|_| Error::Timeout)??;
    let plaintext = opener.open(&sealed)?;
    Ok(serde_json::from_slice(&plaintext)?)
}
