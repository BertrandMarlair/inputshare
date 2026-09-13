//! The networking service: discovery in, connections out, workspace in sync.
//!
//! One actor owns all peer state and runs for the life of the process. Every
//! input — a multicast sighting, a connection result, a command from the app, a
//! timer — arrives on the same channel, so there is exactly one place where
//! peer state changes and no locks between them.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use is_core::{
    Identity, InputEvent, MachineId, PubKey, WorkspaceDoc, WorkspaceId, WorkspaceUpdate,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::discovery::{self, Interface, ManualPeers, Sighting, HINT_TTL};
use crate::session::{self, Outbound, SessionOutcome};
use crate::wire::{Announcement, Message};
use crate::{Error, Result};

/// How the service reaches the workspace document.
///
/// The service deliberately cannot persist anything. Whoever implements this
/// owns the file on disk and decides when to write it, which keeps "what is the
/// workspace" in one place rather than two that can disagree.
pub trait Workspace: Send + Sync + 'static {
    /// The current document, or `None` if this machine has not joined one.
    fn snapshot(&self) -> Option<WorkspaceDoc>;
    /// Merges a peer's document. Returns whether anything changed.
    fn merge(&self, incoming: &WorkspaceDoc) -> is_core::Result<bool>;
    /// Applies one change from a peer. Returns whether anything changed.
    fn apply(&self, update: WorkspaceUpdate) -> is_core::Result<bool>;
    /// This machine's name, for announcements and for the pairing dialog.
    fn display_name(&self) -> String;

    /// `from` says the pointer is now on `machine`. Everyone applies it, so
    /// there is one answer rather than one per machine.
    fn on_cursor_owner(&self, from: MachineId, machine: MachineId) {
        let _ = (from, machine);
    }

    /// Input captured on `from`, to be replayed on this machine.
    ///
    /// Default is to ignore it: a build with no injection support should drop
    /// these rather than pretend.
    fn on_input(&self, from: MachineId, events: Vec<InputEvent>) {
        let _ = (from, events);
    }

    /// Whether a peer that has *proved* it holds `key` is allowed in.
    ///
    /// Proving an identity and being allowed to use it are different questions,
    /// and this is the second one. The default answer is "only machines already
    /// in the workspace, with the key recorded when they were paired".
    ///
    /// It is overridable because of one real case: a machine that has been told
    /// to join a workspace it is not yet a member of. It cannot check the peer
    /// against a document it does not have, so it checks against the decision
    /// the user just made instead. That is still an explicit admission, not an
    /// open door.
    /// Machines this one has admitted but is not connected to, advertised so
    /// they can see they were invited.
    fn pending_invitations(&self) -> Vec<MachineId> {
        Vec::new()
    }

    fn authorize(&self, machine_id: MachineId, key: &PubKey) -> bool {
        self.snapshot()
            .and_then(|doc| doc.machine(machine_id).map(|m| m.public_key))
            .is_some_and(|paired| paired == *key)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub identity: Identity,
    pub discovery_port: u16,
    /// `0` binds an available port, which is what lets two instances run on one
    /// machine during development.
    pub transport_port: u16,
    pub announce_interval: Duration,
}

impl Config {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            discovery_port: 47451,
            transport_port: 47452,
            announce_interval: Duration::from_secs(3),
        }
    }
}

/// What the app learns from the network.
#[derive(Clone, Debug)]
pub enum Event {
    /// A machine in our workspace was heard from.
    Sighted {
        machine_id: MachineId,
    },
    /// A machine we have never paired with is on the network (requirement 15).
    Candidate {
        machine_id: MachineId,
        display_name: String,
        identity_key: PubKey,
        workspace_id: Option<WorkspaceId>,
        addr: SocketAddr,
        /// It has already admitted us and is waiting for us to admit it back.
        invited_us: bool,
    },
    Connecting {
        machine_id: MachineId,
    },
    Online {
        machine_id: MachineId,
    },
    Offline {
        machine_id: MachineId,
        reason: String,
    },
    /// A peer proved an identity we do not trust. Surfaced rather than logged
    /// away: it is either a machine that needs pairing, or someone trying to
    /// join a workspace they are not in.
    Rejected {
        machine_id: MachineId,
        identity_key: PubKey,
        addr: SocketAddr,
    },
    /// A paired machine turned out to be in a different workspace.
    ///
    /// Retrying cannot fix this, so it is reported once per attempt and the peer
    /// is backed off hard until somebody chooses which workspace to keep.
    DifferentWorkspaces {
        machine_id: MachineId,
        display_name: String,
        identity_key: PubKey,
        their_workspace: WorkspaceId,
        /// How many machines their configuration holds, so both sides can pick
        /// the same survivor without asking anybody.
        their_size: u32,
    },
    /// The document changed because of something a peer sent.
    WorkspaceChanged,
}

/// Messages into the actor. One channel for everything that can change state.
pub(crate) enum Internal {
    Sighting(Sighting),
    Session(SessionOutcome),
    Broadcast(Box<WorkspaceUpdate>),
    SendInput {
        to: MachineId,
        events: Vec<InputEvent>,
    },
    AnnounceCursorOwner(MachineId),
    /// Someone asked to look for peers now rather than at the next tick.
    Rescan,
    Tick,
    Shutdown,
}

/// Produces a fresh description of this machine on demand.
type Describe = Arc<dyn Fn() -> Announcement + Send + Sync>;

/// Handle on a running service.
///
/// Cloning is cheap and gives another handle on the same service: it holds only
/// channel senders, which is what lets a caller take a handle out of a lock
/// before awaiting on it.
#[derive(Clone)]
pub struct Service {
    inbox: mpsc::Sender<Internal>,
    events: broadcast::Sender<Event>,
    announce_now: Arc<Notify>,
    manual: ManualPeers,
    discovery_port: u16,
    transport_port: u16,
}

impl Service {
    /// Starts discovery, the listener, and the peer actor.
    pub async fn start(config: Config, workspace: Arc<dyn Workspace>) -> Result<Self> {
        let listener = bind_transport(config.transport_port).await?;
        let transport_port = listener.local_addr().map_err(Error::Io)?.port();

        let (inbox, mailbox) = mpsc::channel(256);
        let (events, _) = broadcast::channel(256);

        // Announcements describe the machine as it is *now*: a rename or a
        // workspace change must not keep advertising the old one.
        let announce_identity = config.identity.machine_id;
        let announce_key = config.identity.public_key();
        let announce_workspace = workspace.clone();
        let announce_now = Arc::new(Notify::new());
        let manual: ManualPeers = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        // One description of this machine, shared by the announce loop and by
        // replies to typed-in addresses, so the two can never disagree.
        let describe: Describe = Arc::new(move || Announcement {
            protocol: crate::crypto::PROTOCOL_VERSION,
            machine_id: announce_identity,
            identity_key: announce_key,
            display_name: announce_workspace.display_name(),
            workspace_id: announce_workspace.snapshot().map(|d| d.workspace_id),
            transport_port,
            probe: false,
            invitations: announce_workspace.pending_invitations(),
        });

        {
            let describe = describe.clone();
            tokio::spawn(discovery::announce(
                config.discovery_port,
                config.announce_interval,
                announce_now.clone(),
                manual.clone(),
                move || describe(),
            ));
        }

        let (sightings, mut sighting_rx) = mpsc::channel(128);
        tokio::spawn(discovery::listen(config.discovery_port, sightings));
        {
            let inbox = inbox.clone();
            tokio::spawn(async move {
                while let Some(sighting) = sighting_rx.recv().await {
                    if inbox.send(Internal::Sighting(sighting)).await.is_err() {
                        break;
                    }
                }
            });
        }

        {
            let inbox = inbox.clone();
            let identity = config.identity.clone();
            let workspace = workspace.clone();
            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, addr)) => {
                            session::spawn_accepted(
                                identity.clone(),
                                workspace.clone(),
                                stream,
                                addr,
                                inbox.clone(),
                            );
                        }
                        Err(error) => {
                            debug!(%error, "transport: accept failed");
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                    }
                }
            });
        }

        {
            let inbox = inbox.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_millis(500));
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    if inbox.send(Internal::Tick).await.is_err() {
                        break;
                    }
                }
            });
        }

        let actor = Actor {
            identity: config.identity,
            workspace,
            events: events.clone(),
            inbox: inbox.clone(),
            peers: HashMap::new(),
            describe,
            discovery_port: config.discovery_port,
        };
        tokio::spawn(actor.run(mailbox));

        info!(port = transport_port, "inputshare: networking up");
        Ok(Self {
            inbox,
            events,
            announce_now,
            manual,
            discovery_port: config.discovery_port,
            transport_port,
        })
    }

    /// The port connections are accepted on. Differs from the configured one
    /// when that was `0`.
    pub fn transport_port(&self) -> u16 {
        self.transport_port
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Pushes a local change to every connected peer (requirement 10).
    pub async fn broadcast(&self, update: WorkspaceUpdate) {
        let _ = self.inbox.send(Internal::Broadcast(Box::new(update))).await;
    }

    /// Sends captured input to one machine.
    ///
    /// Dropped silently if that machine is not connected. Input is only
    /// meaningful the instant it happens: queueing keystrokes for a peer that
    /// might come back would replay them minutes later into whatever window
    /// happens to be focused then.
    pub async fn send_input(&self, to: MachineId, events: Vec<InputEvent>) {
        let _ = self.inbox.send(Internal::SendInput { to, events }).await;
    }

    /// The IPv4 interfaces discovery is using right now.
    ///
    /// Surfaced because the commonest cause of "nothing is found" is that the
    /// only interface in play is a virtual one, and no log line a user will ever
    /// read says so.
    pub fn interfaces(&self) -> Vec<Interface> {
        discovery::interfaces()
    }

    /// Adds an address to try directly, for networks where multicast does not
    /// get through.
    ///
    /// The announcement sent there asks for a reply, so entering an address on
    /// one machine introduces both.
    pub async fn add_manual_peer(&self, addr: SocketAddr) {
        let mut manual = self.manual.lock().await;
        if !manual.contains(&addr) {
            manual.push(addr);
        }
        drop(manual);
        self.announce_now.notify_waiters();
    }

    pub async fn manual_peers(&self) -> Vec<SocketAddr> {
        self.manual.lock().await.clone()
    }

    pub async fn forget_manual_peer(&self, addr: SocketAddr) {
        self.manual
            .lock()
            .await
            .retain(|existing| *existing != addr);
    }

    /// The port announcements are sent to and listened for.
    pub fn discovery_port(&self) -> u16 {
        self.discovery_port
    }

    /// Tells every connected machine where the pointer is now.
    pub async fn announce_cursor_owner(&self, machine: MachineId) {
        let _ = self
            .inbox
            .send(Internal::AnnounceCursorOwner(machine))
            .await;
    }

    /// Announces immediately and clears every reconnection backoff.
    ///
    /// Discovery is automatic, so this changes nothing about what eventually
    /// happens — it only stops the person staring at the screen from having to
    /// wait out an interval, and gives a machine stuck in a long backoff a
    /// fresh chance at once.
    pub async fn rescan(&self) {
        self.announce_now.notify_waiters();
        let _ = self.inbox.send(Internal::Rescan).await;
    }

    pub async fn shutdown(&self) {
        let _ = self.inbox.send(Internal::Shutdown).await;
    }
}

/// What we know about one machine at runtime. None of this is persisted:
/// addresses and liveness are not part of the workspace.
struct PeerSlot {
    display_name: String,
    addr: Option<SocketAddr>,
    /// The workspace this peer last announced.
    ///
    /// Watched because a peer changing workspace is the one event that makes a
    /// backoff obsolete rather than merely early: a connection refused because
    /// the two were in different workspaces is worth retrying the instant either
    /// side leaves or joins one.
    last_workspace: Option<Option<WorkspaceId>>,
    last_seen: Option<Instant>,
    status: Status,
    outbound: Option<Outbound>,
    attempts: u32,
    next_attempt: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Offline,
    Connecting,
    Online,
}

impl PeerSlot {
    fn new(display_name: String) -> Self {
        Self {
            display_name,
            addr: None,
            last_workspace: None,
            last_seen: None,
            status: Status::Offline,
            outbound: None,
            attempts: 0,
            next_attempt: Instant::now(),
        }
    }

    /// Whether we heard from this machine recently enough to believe its
    /// address.
    fn fresh(&self, now: Instant) -> bool {
        self.last_seen
            .is_some_and(|seen| now.duration_since(seen) < HINT_TTL)
    }
}

struct Actor {
    identity: Identity,
    workspace: Arc<dyn Workspace>,
    events: broadcast::Sender<Event>,
    inbox: mpsc::Sender<Internal>,
    peers: HashMap<MachineId, PeerSlot>,
    describe: Describe,
    discovery_port: u16,
}

impl Actor {
    async fn run(mut self, mut mailbox: mpsc::Receiver<Internal>) {
        while let Some(message) = mailbox.recv().await {
            match message {
                Internal::Sighting(sighting) => self.on_sighting(sighting),
                Internal::Session(outcome) => self.on_session(outcome),
                Internal::Broadcast(update) => self.on_broadcast(*update),
                Internal::SendInput { to, events } => self.on_send_input(to, events),
                Internal::AnnounceCursorOwner(machine) => self.on_announce_owner(machine),
                Internal::Rescan => self.on_rescan(),
                Internal::Tick => self.on_tick(),
                Internal::Shutdown => break,
            }
        }
        debug!("inputshare: networking actor stopped");
    }

    fn emit(&self, event: Event) {
        // No subscribers is normal — the app may not be listening yet.
        let _ = self.events.send(event);
    }

    fn on_sighting(&mut self, sighting: Sighting) {
        let announcement = sighting.announcement;
        if announcement.machine_id == self.identity.machine_id {
            return; // our own multicast, looped back
        }

        let addr = SocketAddr::new(sighting.from.ip(), announcement.transport_port);

        // Somebody typed our address in over there. Answer once, directly, so
        // they appear on this machine too instead of only the other way round.
        if announcement.probe {
            let reply = (self.describe)();
            let port = self.discovery_port;
            let to = sighting.from.ip();
            tokio::spawn(async move {
                discovery::reply_to_probe(port, to, &reply).await;
            });
        }

        // The same question the handshake will ask later, asked early so we know
        // whether this is a peer to connect to or a machine to offer the user.
        //
        // It has to be that exact question. Checking the workspace document
        // instead would leave out a machine that has been told to *join* one —
        // it has no document yet, so it would never record the peer, never dial,
        // and the pair would only connect when the other side happened to have
        // the lower machine ID.
        if !self
            .workspace
            .authorize(announcement.machine_id, &announcement.identity_key)
        {
            // Not one of ours. Offer it for pairing and do nothing else: an
            // unpaired machine must not be able to make us open connections.
            let invited_us = announcement.invitations.contains(&self.identity.machine_id);
            self.emit(Event::Candidate {
                machine_id: announcement.machine_id,
                display_name: announcement.display_name,
                identity_key: announcement.identity_key,
                workspace_id: announcement.workspace_id,
                addr,
                invited_us,
            });
            return;
        }

        let slot = self
            .peers
            .entry(announcement.machine_id)
            .or_insert_with(|| PeerSlot::new(announcement.display_name.clone()));
        slot.display_name = announcement.display_name;
        slot.addr = Some(addr);
        let now = Instant::now();
        let was_stale = !slot.fresh(now);
        let changed_workspace = slot
            .last_workspace
            .is_some_and(|seen| seen != announcement.workspace_id);
        slot.last_workspace = Some(announcement.workspace_id);
        slot.last_seen = Some(now);
        if was_stale || changed_workspace {
            // A machine that just came back should not wait out a backoff it
            // earned while it was away (requirement 6) — and neither should one
            // that just resolved whatever made the last attempt pointless.
            slot.attempts = 0;
            slot.next_attempt = now;
        }

        self.emit(Event::Sighted {
            machine_id: announcement.machine_id,
        });
    }

    fn on_session(&mut self, outcome: SessionOutcome) {
        match outcome {
            SessionOutcome::Established {
                machine_id,
                display_name,
                outbound,
            } => {
                let slot = self
                    .peers
                    .entry(machine_id)
                    .or_insert_with(|| PeerSlot::new(display_name.clone()));
                slot.display_name = display_name;
                slot.status = Status::Online;
                slot.outbound = Some(outbound);
                slot.attempts = 0;
                slot.last_seen = Some(Instant::now());
                info!(machine = %machine_id, "peer online");
                self.emit(Event::Online { machine_id });
            }
            SessionOutcome::Closed { machine_id, reason } => {
                if let Some(slot) = self.peers.get_mut(&machine_id) {
                    let was_online = slot.status == Status::Online;
                    slot.status = Status::Offline;
                    slot.outbound = None;
                    slot.attempts = slot.attempts.saturating_add(1);
                    slot.next_attempt = Instant::now() + backoff(slot.attempts);
                    if was_online {
                        info!(machine = %machine_id, %reason, "peer offline");
                    }
                }
                self.emit(Event::Offline { machine_id, reason });
            }
            SessionOutcome::Rejected {
                machine_id,
                identity_key,
                addr,
            } => {
                warn!(machine = %machine_id, "refused a peer we have not paired with");
                self.emit(Event::Rejected {
                    machine_id,
                    identity_key,
                    addr,
                });
            }
            SessionOutcome::DifferentWorkspaces {
                machine_id,
                display_name,
                identity_key,
                their_workspace,
                their_size,
            } => {
                if let Some(slot) = self.peers.get_mut(&machine_id) {
                    slot.status = Status::Offline;
                    slot.outbound = None;
                    // Nothing to gain from trying again soon: this needs a
                    // person to decide which workspace survives.
                    slot.attempts = slot.attempts.max(5);
                    slot.next_attempt = Instant::now() + Duration::from_secs(30);
                }
                warn!(machine = %machine_id, "peer is in a different workspace");
                self.emit(Event::DifferentWorkspaces {
                    machine_id,
                    display_name,
                    identity_key,
                    their_workspace,
                    their_size,
                });
            }
            SessionOutcome::WorkspaceChanged => self.emit(Event::WorkspaceChanged),
        }
    }

    /// Pushes a local change to every connected peer.
    ///
    /// Never waits on a peer. If one has stopped draining its queue, its
    /// connection is dropped instead: reconnecting re-exchanges whole documents,
    /// whereas quietly discarding an update would leave the two machines
    /// disagreeing with nothing to notice it.
    fn on_broadcast(&mut self, update: WorkspaceUpdate) {
        for (machine_id, slot) in self.peers.iter_mut() {
            let Some(outbound) = &slot.outbound else {
                continue;
            };
            let message = Message::Update {
                update: Box::new(update.clone()),
            };
            if outbound.try_send(message).is_err() {
                debug!(machine = %machine_id, "peer is not keeping up, dropping the connection");
                slot.outbound = None;
            }
        }
    }

    fn on_announce_owner(&mut self, machine: MachineId) {
        for slot in self.peers.values_mut() {
            if let Some(outbound) = &slot.outbound {
                let _ = outbound.try_send(Message::CursorOwner { machine });
            }
        }
    }

    fn on_send_input(&mut self, to: MachineId, events: Vec<InputEvent>) {
        let Some(slot) = self.peers.get_mut(&to) else {
            return;
        };
        let Some(outbound) = &slot.outbound else {
            return;
        };
        if outbound.try_send(Message::Input { events }).is_err() {
            // The peer stopped draining. Drop the connection rather than let
            // input pile up: a reconnect costs a second, a queue of stale
            // keystrokes arriving late is worse than losing them.
            debug!(machine = %to, "peer is not keeping up with input, dropping the connection");
            slot.outbound = None;
        }
    }

    /// Wipes the backoff on every peer that is not connected, so the next tick
    /// retries all of them at once.
    fn on_rescan(&mut self) {
        let now = Instant::now();
        for slot in self.peers.values_mut() {
            if slot.status == Status::Offline {
                slot.attempts = 0;
                slot.next_attempt = now;
            }
        }
        debug!("rescan requested");
    }

    /// Dials the peers that are due.
    ///
    /// Two rules keep this cheap, which is what requirement 7 asks for. A
    /// machine is only dialled if it was heard from recently, so a powered-off
    /// machine costs nothing at all — no timeouts, no retry storm, no log spam.
    /// And of any two machines, only the one with the lower ID dials, so they
    /// never race into two connections that then have to be torn down.
    fn on_tick(&mut self) {
        let now = Instant::now();
        let me = self.identity.machine_id;

        let due: Vec<(MachineId, SocketAddr)> = self
            .peers
            .iter()
            .filter(|(machine_id, slot)| {
                slot.status == Status::Offline
                    && me < **machine_id
                    && slot.fresh(now)
                    && now >= slot.next_attempt
            })
            .filter_map(|(machine_id, slot)| slot.addr.map(|addr| (*machine_id, addr)))
            .collect();

        for (machine_id, addr) in due {
            let Some(slot) = self.peers.get_mut(&machine_id) else {
                continue;
            };
            slot.status = Status::Connecting;
            self.emit(Event::Connecting { machine_id });
            session::spawn_dial(
                self.identity.clone(),
                self.workspace.clone(),
                machine_id,
                addr,
                self.inbox.clone(),
            );
        }

        // Forget addresses for machines that stopped announcing, so a stale
        // address is never dialled after a DHCP change.
        for (machine_id, slot) in self.peers.iter_mut() {
            if slot.status == Status::Offline && !slot.fresh(now) && slot.addr.take().is_some() {
                debug!(machine = %machine_id, "discovery: address hint expired");
            }
        }
    }
}

/// Exponential with a hard cap. The cap matters: an unreachable-but-announcing
/// machine must settle into a slow poll rather than hammering forever.
fn backoff(attempts: u32) -> Duration {
    const CAP: Duration = Duration::from_secs(30);
    let step = Duration::from_secs(1) * 2u32.saturating_pow(attempts.min(5));
    step.min(CAP)
}

/// Binds the transport listener, falling back to any free port.
///
/// A busy port is not a reason to have no networking at all: another copy of
/// the app, or something unrelated, may already hold the configured one. The
/// real port is published in the announcements, so peers find it regardless.
async fn bind_transport(preferred: u16) -> Result<TcpListener> {
    let any = IpAddr::from([0, 0, 0, 0]);
    match TcpListener::bind((any, preferred)).await {
        Ok(listener) => Ok(listener),
        Err(error) if preferred != 0 && error.kind() == std::io::ErrorKind::AddrInUse => {
            warn!(
                port = preferred,
                "transport port is taken, using a free one"
            );
            TcpListener::bind((any, 0)).await.map_err(Error::Io)
        }
        Err(error) => Err(Error::Io(error)),
    }
}

/// Opens a connection to `addr` and runs a session on it.
pub(crate) async fn dial(addr: SocketAddr) -> Result<TcpStream> {
    tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(Error::Io)
}
