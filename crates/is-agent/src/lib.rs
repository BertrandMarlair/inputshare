#![forbid(unsafe_code)]

//! The agent: what this machine knows and does about its workspace.
//!
//! It owns the three pieces that must not disagree — the document on disk, the
//! live peer connections, and who is reachable right now — and it is the only
//! thing allowed to change any of them. The desktop app is a view onto it.
//!
//! Requirement 4 wants the input agent to run without the UI open. This crate is
//! that agent as a library: today the window hosts it in-process, and the
//! headless daemon wraps the same type without the window learning anything new.

pub mod autostart;
pub mod sharing;

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use is_core::{
    Identity, InputEvent, MachineId, MachineRecord, PubKey, Store, WorkspaceDoc, WorkspaceSettings,
    WorkspaceUpdate,
};
use is_net::{Config, Event as NetEvent, Service};
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

pub type Result<T> = std::result::Result<T, Error>;

/// Where input captured on another machine is delivered.
type RemoteInput = tokio::sync::mpsc::Sender<(MachineId, Vec<InputEvent>)>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Core(#[from] is_core::Error),

    #[error("network: {0}")]
    Net(#[from] is_net::Error),

    #[error("this machine has not joined a workspace yet")]
    NoWorkspace,

    #[error("no machine with id {0}")]
    UnknownMachine(MachineId),

    #[error("{0}")]
    Refused(String),
}

/// A machine seen on the network that is not in our workspace (requirement 15).
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub machine_id: MachineId,
    pub display_name: String,
    pub identity_key: PubKey,
    /// It has already admitted this machine and is waiting to be admitted back.
    /// One click finishes the pairing instead of two blind ones.
    pub invited_us: bool,
    /// Where it was last heard from. Shown because "found nothing" and "found
    /// something on the wrong network" look identical without it.
    pub addr: String,
    pub last_seen_ms: u64,
    /// Whether it already belongs to some workspace. A machine that is in one
    /// and a machine that is fresh out of the box are different situations for
    /// the person looking at the screen.
    pub has_workspace: bool,
}

/// Something changed and any view should refresh.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    Changed,
}

struct State {
    doc: Option<WorkspaceDoc>,
    /// Peers with a live authenticated session. Never persisted: liveness is not
    /// part of the workspace (requirement 3).
    online: HashSet<MachineId>,
    candidates: BTreeMap<MachineId, Candidate>,
    /// Machines the user admitted before this computer had a workspace to put
    /// them in — the "join an existing workspace" case from requirement 13.
    ///
    /// A machine joining cannot check a peer against a document it does not
    /// have yet, so until it adopts one it checks against this instead. Each
    /// entry is a deliberate decision by the person at the keyboard, and it is
    /// dropped as soon as the real record arrives.
    pending_trust: BTreeMap<MachineId, PubKey>,
    /// Addresses typed in by hand. Mirrored here as well as in the network
    /// service so a view can read them without awaiting.
    manual_peers: Vec<std::net::SocketAddr>,
}

/// The part the network talks to. Separate from [`Agent`] so `is-net` can hold
/// it without holding the service that owns it.
pub(crate) struct Core {
    store: Store,
    identity: Identity,
    state: Mutex<State>,
    events: broadcast::Sender<AgentEvent>,
    /// The last ownership announcement received, waiting to be applied by the
    /// sharing loop.
    pub(crate) announced_owner: Mutex<Option<MachineId>>,
    /// Where input from peers goes while sharing is running.
    ///
    /// Routed rather than injected outright: the machine holding the pointer has
    /// to see another machine's movements *as movements*, or a pointer being
    /// driven by the other mouse can never cross back.
    remote_input: Mutex<Option<RemoteInput>>,
    /// Which machine the pointer is actually on, as the sharing loop sees it.
    ///
    /// Worth publishing: with two mice in play, "where is the cursor" is the
    /// question a person asks first, and the alternative to showing it is
    /// waggling a mouse to find out.
    pub(crate) cursor_owner: Mutex<Option<MachineId>>,
    /// Whether this machine is taking part in input sharing at all.
    ///
    /// Gates both directions. A machine that is not sharing neither captures its
    /// own keyboard nor replays anyone else's — being in a workspace is not on
    /// its own consent to have keystrokes injected.
    pub(crate) sharing: AtomicBool,
}

impl Core {
    /// Persists the document. Every mutation goes through here, so the file on
    /// disk is never behind what the app is showing.
    fn persist(&self, state: &State) {
        if let Some(doc) = &state.doc {
            if let Err(error) = self.store.save_workspace(doc) {
                // Losing the write is bad, but taking the app down with it is
                // worse: the workspace still works, it just did not survive a
                // reboot, and the next change will try again.
                warn!(%error, "could not save the workspace");
            }
        }
    }

    fn announce_change(&self) {
        let _ = self.events.send(AgentEvent::Changed);
    }
}

impl is_net::Workspace for Core {
    fn snapshot(&self) -> Option<WorkspaceDoc> {
        self.state.lock().ok()?.doc.clone()
    }

    fn merge(&self, incoming: &WorkspaceDoc) -> is_core::Result<bool> {
        let mut state = self.state.lock().expect("agent state");
        let changed = match state.doc.as_mut() {
            Some(doc) => doc.merge(incoming)?,
            None => {
                // A machine that was reinstalled, or paired from the other end,
                // adopts the workspace it is being told about.
                state.doc = Some(incoming.clone());
                true
            }
        };
        if changed {
            // Anything now recorded in the document is trusted on its own terms.
            let adopted: Vec<MachineId> = state
                .pending_trust
                .keys()
                .copied()
                .filter(|id| {
                    state
                        .doc
                        .as_ref()
                        .is_some_and(|doc| doc.machine(*id).is_some())
                })
                .collect();
            for machine_id in adopted {
                state.pending_trust.remove(&machine_id);
            }
            self.persist(&state);
            drop(state);
            self.announce_change();
        }
        Ok(changed)
    }

    fn apply(&self, update: WorkspaceUpdate) -> is_core::Result<bool> {
        let mut state = self.state.lock().expect("agent state");
        let Some(doc) = state.doc.as_mut() else {
            return Ok(false);
        };
        let changed = doc.apply(update)?;
        if changed {
            self.persist(&state);
            drop(state);
            self.announce_change();
        }
        Ok(changed)
    }

    fn display_name(&self) -> String {
        let state = self.state.lock().expect("agent state");
        state
            .doc
            .as_ref()
            .and_then(|doc| doc.machine(self.identity.machine_id))
            .map(|m| m.display_name.clone())
            .unwrap_or_else(computer_name)
    }

    fn on_cursor_owner(&self, from: MachineId, machine: MachineId) {
        debug!(machine = %from, owner = %machine, "told where the pointer is");
        *self.announced_owner.lock().expect("announced owner") = Some(machine);
    }

    fn on_input(&self, from: MachineId, events: Vec<InputEvent>) {
        // While sharing is running the routing loop owns this; it decides what
        // reaches the desktop and what means "the pointer is leaving".
        if let Some(sink) = self.remote_input.lock().expect("remote input").as_ref() {
            if sink.try_send((from, events)).is_err() {
                debug!("input from a peer arrived faster than it could be routed");
            }
            return;
        }

        // Deliberately not gated on this machine's own sharing switch.
        //
        // Requiring it on both sides recreates the worst failure this product
        // has: a person flips one switch, nothing happens, and nothing on either
        // screen says why. The switch means "capture this keyboard and send it";
        // accepting input is what pairing already agreed to — the peer is
        // authenticated, admitted by hand, and talking over an encrypted link.
        let _ = from;
        for event in events {
            if let Err(error) = is_input::inject::inject(event) {
                debug!(%error, "could not replay an input event");
                break;
            }
        }
    }

    /// Machines in the workspace that are not connected right now.
    ///
    /// Advertised so a machine this one has admitted can see it was invited,
    /// rather than both people clicking Pair into the void and wondering which
    /// of them failed.
    fn pending_invitations(&self) -> Vec<MachineId> {
        let state = self.state.lock().expect("agent state");
        let Some(doc) = state.doc.as_ref() else {
            return Vec::new();
        };
        doc.machines_present()
            .map(|m| m.id)
            .filter(|id| *id != self.identity.machine_id && !state.online.contains(id))
            .collect()
    }

    fn authorize(&self, machine_id: MachineId, key: &PubKey) -> bool {
        let state = self.state.lock().expect("agent state");
        if let Some(paired) = state
            .doc
            .as_ref()
            .and_then(|doc| doc.machine(machine_id).map(|m| m.public_key))
        {
            return paired == *key;
        }
        // No record for this machine: the only other way in is an explicit
        // decision to join it, made by the user before there was a document.
        state.pending_trust.get(&machine_id) == Some(key)
    }
}

/// Everything a view needs, in one snapshot.
pub struct View {
    pub identity: Identity,
    pub doc: Option<WorkspaceDoc>,
    pub online: HashSet<MachineId>,
    pub candidates: Vec<Candidate>,
    /// Machines this computer has asked to join, still waiting to be admitted
    /// from the other end.
    pub awaiting_join: Vec<MachineId>,
    pub manual_peers: Vec<std::net::SocketAddr>,
    /// The machine the pointer is on, when input is being shared.
    pub cursor_owner: Option<MachineId>,
    pub config_dir: std::path::PathBuf,
    pub transport_port: Option<u16>,
}

pub struct Agent {
    core: Arc<Core>,
    net: Mutex<Option<Service>>,
    sharing: Mutex<Option<sharing::Sharing>>,
}

impl Agent {
    /// Loads this machine's identity and configuration, creating one on first
    /// launch. Does not touch the network.
    ///
    /// There is no setup step. A computer on its own is a perfectly good
    /// configuration of one machine, and asking somebody to name a "workspace"
    /// before anything works is a question about the data model rather than
    /// about what they want.
    pub fn load(store: Store) -> Result<Arc<Self>> {
        let identity = store.load_or_create_identity()?;
        let doc = match store.load_workspace()? {
            Some(doc) => Some(doc),
            None => {
                let doc = WorkspaceDoc::create(computer_name(), local_record(&identity));
                store.save_workspace(&doc)?;
                info!("first launch: created this machine's configuration");
                Some(doc)
            }
        };
        let (events, _) = broadcast::channel(64);

        Ok(Arc::new(Self {
            core: Arc::new(Core {
                store,
                identity,
                state: Mutex::new(State {
                    doc,
                    online: HashSet::new(),
                    candidates: BTreeMap::new(),
                    pending_trust: BTreeMap::new(),
                    manual_peers: Vec::new(),
                }),
                events,
                sharing: AtomicBool::new(false),
                announced_owner: Mutex::new(None),
                remote_input: Mutex::new(None),
                cursor_owner: Mutex::new(None),
            }),
            net: Mutex::new(None),
            sharing: Mutex::new(None),
        }))
    }

    /// Starts discovery and peer connections.
    ///
    /// Separate from [`Agent::load`] so the app can show a workspace instantly
    /// and come online a moment later, rather than blocking the window on a
    /// socket.
    pub async fn go_online(self: &Arc<Self>) -> Result<u16> {
        let settings = self
            .core
            .state
            .lock()
            .expect("agent state")
            .doc
            .as_ref()
            .map(|doc| doc.settings.value.clone())
            .unwrap_or_default();
        self.go_online_on(settings.discovery_port, settings.transport_port)
            .await
    }

    /// Same, on explicit ports. Used by tests, and by a second instance on one
    /// machine that must not fight the first for the well-known ports.
    pub async fn go_online_on(
        self: &Arc<Self>,
        discovery_port: u16,
        transport_port: u16,
    ) -> Result<u16> {
        let mut config = Config::new(self.core.identity.clone());
        config.discovery_port = discovery_port;
        config.transport_port = transport_port;

        let service = Service::start(config, self.core.clone()).await?;
        let port = service.transport_port();
        let mut events = service.subscribe();
        *self.net.lock().expect("net slot") = Some(service);

        let agent = self.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => on_net_event(&agent, event).await,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        debug!(missed, "agent: fell behind network events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // A machine that joins adopts a document in which its own record was
        // created by somebody else, with no screens in it — and a record with no
        // screens has no geometry, so the machine shows up in the workspace as
        // nothing at all. Re-publishing on every change fixes that, and also
        // covers a monitor being plugged in later.
        let refresher = self.clone();
        tokio::spawn(async move {
            let mut changes = refresher.subscribe();
            while changes.recv().await.is_ok() {
                if let Err(error) = refresher.publish_local_facts().await {
                    debug!(%error, "could not publish this machine's displays");
                }
            }
        });

        info!(port, "agent online");
        Ok(port)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.core.events.subscribe()
    }

    pub fn identity(&self) -> &Identity {
        &self.core.identity
    }

    pub fn view(&self) -> View {
        let state = self.core.state.lock().expect("agent state");
        View {
            identity: self.core.identity.clone(),
            doc: state.doc.clone(),
            online: state.online.clone(),
            candidates: state.candidates.values().cloned().collect(),
            awaiting_join: state.pending_trust.keys().copied().collect(),
            manual_peers: state.manual_peers.clone(),
            cursor_owner: *self.core.cursor_owner.lock().expect("cursor owner"),
            config_dir: self.core.store.dir().to_path_buf(),
            transport_port: self
                .net
                .lock()
                .ok()
                .and_then(|slot| slot.as_ref().map(|s| s.transport_port())),
        }
    }

    /// Renames this machine, as every other computer sees it.
    pub async fn rename_this_machine(self: &Arc<Self>, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Refused("a computer needs a name".into()));
        }
        let me = self.core.identity.machine_id;
        let mut record = self
            .view()
            .doc
            .and_then(|doc| doc.machine(me).cloned())
            .ok_or(Error::UnknownMachine(me))?;
        record.display_name = name.to_string();
        self.upsert_machine(record).await
    }

    /// Admits a discovered machine into the workspace.
    ///
    /// This records its public key, which is what every later connection is
    /// checked against. The other machine has to do the same for us: neither
    /// side will talk to a machine it has not been told to trust, and that is
    /// the property that makes an open network safe to run this on.
    pub async fn pair(self: &Arc<Self>, machine_id: MachineId) -> Result<()> {
        let candidate = {
            let state = self.core.state.lock().expect("agent state");
            state
                .candidates
                .get(&machine_id)
                .cloned()
                .ok_or(Error::UnknownMachine(machine_id))?
        };

        // No workspace of our own yet: this is a join, not an admission. Trust
        // the machine provisionally and adopt its workspace when it connects,
        // rather than creating a second workspace that could never merge with
        // the first.
        if self.core.state.lock().expect("agent state").doc.is_none() {
            let mut state = self.core.state.lock().expect("agent state");
            state
                .pending_trust
                .insert(candidate.machine_id, candidate.identity_key);
            drop(state);
            self.core.announce_change();
            info!(machine = %machine_id, "waiting to join this machine's workspace");
            return Ok(());
        }

        let position = self.free_position();
        let record = MachineRecord {
            id: candidate.machine_id,
            display_name: candidate.display_name.clone(),
            // The machine reports its own platform and displays once connected.
            platform: is_core::Platform::Other,
            public_key: candidate.identity_key,
            position,
            displays: Vec::new(),
            input: is_core::InputAssignment::default(),
            paired_at_ms: is_core::now_ms(),
        };

        self.mutate(|doc, origin| doc.upsert_machine(origin, record.clone()))
            .await?;
        self.core
            .state
            .lock()
            .expect("agent state")
            .candidates
            .remove(&machine_id);
        info!(machine = %machine_id, "paired");
        Ok(())
    }

    pub async fn unpair(self: &Arc<Self>, machine_id: MachineId) -> Result<()> {
        if machine_id == self.core.identity.machine_id {
            return Err(Error::Refused(
                "a machine cannot remove itself from its own workspace".into(),
            ));
        }
        self.mutate(|doc, origin| doc.remove_machine(origin, machine_id))
            .await?;
        let mut state = self.core.state.lock().expect("agent state");
        state.online.remove(&machine_id);
        Ok(())
    }

    pub async fn upsert_machine(self: &Arc<Self>, record: MachineRecord) -> Result<()> {
        self.mutate(|doc, origin| doc.upsert_machine(origin, record.clone()))
            .await
    }

    /// Whether this machine is sharing its keyboard and mouse.
    pub fn is_sharing(&self) -> bool {
        self.core.sharing.load(Ordering::Relaxed)
    }

    /// Turns input sharing on or off.
    ///
    /// Off by default, and off again the moment this returns `false`. While it
    /// is on, this process can swallow the local keyboard — see [`sharing`] for
    /// why that is treated as carefully as it is.
    ///
    /// Async because starting it spawns the routing task, and spawning needs a
    /// runtime to be entered. It used to be synchronous, which meant calling it
    /// from anywhere but inside the runtime killed the process on the spot.
    pub async fn set_sharing(self: &Arc<Self>, enabled: bool) -> Result<()> {
        // The choice is written down before it is acted on, and whether or not
        // it changes anything right now. If starting capture fails — no
        // permission yet on macOS, hooks refused — the machine still wants to
        // share, and should try again next launch rather than silently forget.
        self.remember_sharing(enabled);
        if enabled == self.is_sharing() {
            return Ok(());
        }
        if enabled {
            let handle = sharing::start(self.clone())?;
            *self.sharing.lock().expect("sharing slot") = Some(handle);
            self.core.sharing.store(true, Ordering::Relaxed);
        } else {
            self.core.sharing.store(false, Ordering::Relaxed);
            // Taken out of the lock first: waiting for the hooks to be gone is
            // async, and a std mutex guard cannot cross an await.
            let handle = self.sharing.lock().expect("sharing slot").take();
            if let Some(handle) = handle {
                handle.stop().await;
            }
            self.note_cursor_owner(None);
        }
        self.core.announce_change();
        Ok(())
    }

    /// Whether this machine had sharing on when it was last used.
    ///
    /// The app calls [`Agent::set_sharing`] with this once it is online, which
    /// is what makes a reboot resume sharing instead of leaving the switch off
    /// and the machines looking connected but inert.
    pub fn sharing_was_on(&self) -> bool {
        self.core.store.load_prefs().sharing
    }

    /// Writes the sharing choice to this machine's own preferences file.
    ///
    /// A failure here is logged and swallowed: not being able to write a
    /// preference is no reason to refuse to share the keyboard.
    fn remember_sharing(&self, enabled: bool) {
        let mut prefs = self.core.store.load_prefs();
        if prefs.sharing == enabled {
            return;
        }
        prefs.sharing = enabled;
        if let Err(error) = self.core.store.save_prefs(&prefs) {
            warn!(%error, "could not remember the sharing switch");
        }
    }

    /// Records where the pointer is, for anything showing it.
    pub(crate) fn note_cursor_owner(&self, owner: Option<MachineId>) {
        let mut slot = self.core.cursor_owner.lock().expect("cursor owner");
        if *slot != owner {
            *slot = owner;
            drop(slot);
            self.core.announce_change();
        }
    }

    /// Points peer input at the routing loop, or back at direct injection.
    pub(crate) fn set_remote_input_sink(&self, sink: Option<RemoteInput>) {
        *self.core.remote_input.lock().expect("remote input") = sink;
    }

    /// An ownership announcement that has arrived and not yet been applied.
    pub(crate) fn take_announced_owner(&self) -> Option<MachineId> {
        self.core
            .announced_owner
            .lock()
            .expect("announced owner")
            .take()
    }

    /// Tells every connected machine where the pointer is now.
    pub(crate) async fn announce_cursor_owner(&self, machine: MachineId) {
        let service = self.net.lock().expect("net slot").clone();
        if let Some(service) = service {
            service.announce_cursor_owner(machine).await;
        }
    }

    /// Sends captured input to whichever machine currently holds the cursor.
    pub(crate) async fn send_input(&self, to: MachineId, events: Vec<InputEvent>) {
        let service = self.net.lock().expect("net slot").clone();
        if let Some(service) = service {
            service.send_input(to, events).await;
        }
    }

    /// The network interfaces discovery is using.
    pub fn interfaces(&self) -> Vec<is_net::discovery::Interface> {
        let service = self.net.lock().expect("net slot").clone();
        match service {
            Some(service) => service.interfaces(),
            None => is_net::discovery::interfaces(),
        }
    }

    /// Adds an address to contact directly, for networks where multicast does
    /// not get through.
    pub async fn add_manual_peer(self: &Arc<Self>, addr: std::net::SocketAddr) {
        {
            let mut state = self.core.state.lock().expect("agent state");
            if !state.manual_peers.contains(&addr) {
                state.manual_peers.push(addr);
            }
        }
        let service = self.net.lock().expect("net slot").clone();
        if let Some(service) = service {
            service.add_manual_peer(addr).await;
        }
        self.core.announce_change();
    }

    pub async fn forget_manual_peer(self: &Arc<Self>, addr: std::net::SocketAddr) {
        self.core
            .state
            .lock()
            .expect("agent state")
            .manual_peers
            .retain(|existing| *existing != addr);
        let service = self.net.lock().expect("net slot").clone();
        if let Some(service) = service {
            service.forget_manual_peer(addr).await;
        }
        self.core.announce_change();
    }

    /// The port announcements go to. Needed when someone types in an address
    /// without one.
    pub fn discovery_port(&self) -> u16 {
        self.view()
            .doc
            .map(|doc| doc.settings.value.discovery_port)
            .unwrap_or(47451)
    }

    /// Gives up this machine's configuration to join the one `machine_id` is in.
    ///
    /// The only way out when both computers created their own workspace and then
    /// paired: they can authenticate each other perfectly and still never
    /// connect, because two workspaces cannot merge.
    ///
    /// Destructive, and deliberately so — the local layout and settings are
    /// replaced by the other machine's. A copy of what is being given up is
    /// written next to the configuration first, so it is not simply gone.
    pub async fn adopt_configuration_of(
        self: &Arc<Self>,
        machine_id: MachineId,
        their_key: PubKey,
    ) -> Result<()> {
        let rescue = self
            .core
            .store
            .dir()
            .join(format!("workspace-replaced-{}.json", is_core::now_ms()));
        if let Err(error) = self.core.store.export_backup(&rescue) {
            warn!(%error, "could not save a copy of the workspace being replaced");
        } else {
            info!(file = %rescue.display(), "saved a copy of the replaced workspace");
        }

        {
            let mut state = self.core.state.lock().expect("agent state");
            state.doc = None;
            state.online.remove(&machine_id);
            // Keep trusting them across the gap: with no document there is
            // nothing to check the next connection against, and this is the
            // decision that says they are welcome.
            state.pending_trust.insert(machine_id, their_key);
        }
        self.core.store.clear_workspace()?;
        self.core.announce_change();

        // A peer that reported a workspace mismatch was pushed into a long
        // backoff, because retrying could not have helped. It can now, and
        // making the person wait out a timer they cannot see is the wrong end of
        // that trade.
        self.rescan().await;

        info!(machine = %machine_id, "gave up this workspace to join theirs");
        Ok(())
    }

    /// Asks the network to look for peers right now.
    pub async fn rescan(self: &Arc<Self>) {
        let service = self.net.lock().expect("net slot").clone();
        if let Some(service) = service {
            service.rescan().await;
        }
    }

    /// Publishes the facts only this machine knows about itself: its screens and
    /// its platform.
    ///
    /// Both matter because a record created by *pairing* is written by the other
    /// computer, which knows neither. Until this runs, the machine sits in the
    /// workspace with no size and an unknown platform — invisible on the canvas
    /// and impossible to route a cursor to.
    ///
    /// It writes only when something actually differs. The document is shared,
    /// so a needless rewrite bumps the revision on every peer and costs a
    /// broadcast to all of them.
    ///
    /// Returns whether anything was published.
    pub async fn publish_local_facts(self: &Arc<Self>) -> Result<bool> {
        let displays = local_displays();

        let me = self.core.identity.machine_id;
        let Some(mut record) = self.view().doc.and_then(|doc| doc.machine(me).cloned()) else {
            return Ok(false);
        };

        let platform = is_core::Platform::current();
        let screens_changed = is_input::displays::differ(&record.displays, &displays);
        if !screens_changed && record.platform == platform {
            return Ok(false);
        }

        info!(
            screens = displays.len(),
            "publishing what only this machine knows about itself"
        );
        if screens_changed {
            record.displays = displays;
        }
        record.platform = platform;
        self.upsert_machine(record).await?;
        Ok(true)
    }

    pub async fn rename_workspace(self: &Arc<Self>, name: &str) -> Result<()> {
        let name = name.to_string();
        self.mutate(|doc, origin| doc.set_name(origin, name.clone()))
            .await
    }

    pub async fn update_settings(self: &Arc<Self>, settings: WorkspaceSettings) -> Result<()> {
        self.mutate(|doc, origin| doc.set_settings(origin, settings.clone()))
            .await
    }

    /// Applies a change locally, persists it, then pushes it to every connected
    /// peer (requirement 10).
    async fn mutate(
        self: &Arc<Self>,
        change: impl Fn(&mut WorkspaceDoc, MachineId) -> WorkspaceUpdate,
    ) -> Result<()> {
        let update = {
            let mut state = self.core.state.lock().expect("agent state");
            let origin = self.core.identity.machine_id;
            let doc = state.doc.as_mut().ok_or(Error::NoWorkspace)?;
            let update = change(doc, origin);
            self.core.persist(&state);
            update
        };

        self.core.announce_change();

        // Taken out of the lock before awaiting: a std mutex guard cannot cross
        // an await, and holding one across network work would stall every other
        // caller anyway.
        let service = self.net.lock().expect("net slot").clone();
        if let Some(service) = service {
            service.broadcast(update).await;
        }
        Ok(())
    }

    /// Somewhere to put a newly paired machine that does not sit on top of one
    /// that is already placed. The user drags it where it belongs.
    fn free_position(&self) -> is_core::Point {
        let state = self.core.state.lock().expect("agent state");
        let right = state
            .doc
            .as_ref()
            .and_then(|doc| {
                doc.machines_present()
                    .filter_map(|m| m.bounds())
                    .map(|b| b.max_x())
                    .max()
            })
            .unwrap_or(0);
        is_core::Point::new(right, 0)
    }
}

/// This machine's screens, or one stand-in when the platform cannot say.
///
/// A machine with no screens has no size, and a machine with no size cannot be
/// placed on the canvas or have a pointer routed to it — it is in the workspace
/// and completely unusable. That is the state every platform without display
/// enumeration would be in, so they get a rectangle and an honest label instead.
fn local_displays() -> Vec<is_core::DisplayInfo> {
    match is_input::displays::enumerate() {
        Ok(displays) if !displays.is_empty() => displays,
        _ => vec![is_core::DisplayInfo {
            id: "assumed".into(),
            name: "Screen (not detected)".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }],
    }
}

/// This machine as a record, with whatever the operating system can tell us
/// about it right now.
fn local_record(identity: &Identity) -> MachineRecord {
    MachineRecord {
        id: identity.machine_id,
        display_name: computer_name(),
        platform: is_core::Platform::current(),
        public_key: identity.public_key(),
        position: is_core::Point::new(0, 0),
        displays: local_displays(),
        input: is_core::InputAssignment {
            provides_keyboard: true,
            provides_mouse: true,
        },
        paired_at_ms: is_core::now_ms(),
    }
}

/// What this computer is called, for a machine that has not joined a workspace
/// yet.
///
/// It ends up as the name the *other* machine records when it admits this one,
/// so a technical placeholder here becomes a permanent label over there.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Unnamed computer".to_string())
}

/// Whether this machine should give up its configuration for the peer's.
///
/// Both sides run this on the same two numbers and exactly one of them yields:
/// the smaller configuration gives way, and an even split is broken by
/// identifier. It is arbitrary, which is the point — any rule works as long as
/// both compute it identically and neither asks a person to arbitrate something
/// that is an artifact of how the data is stored.
fn should_yield(
    ours: &WorkspaceDoc,
    their_workspace: is_core::WorkspaceId,
    their_size: u32,
) -> bool {
    let our_size = ours.machines_present().count() as u32;
    match our_size.cmp(&their_size) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => ours.workspace_id > their_workspace,
    }
}

async fn on_net_event(agent: &Arc<Agent>, event: NetEvent) {
    // Handled before the lock: it needs to await, and it is the one event that
    // changes which configuration this machine is in.
    if let NetEvent::DifferentWorkspaces {
        machine_id,
        identity_key,
        their_workspace,
        their_size,
        ..
    } = &event
    {
        let ours = agent.view().doc;
        if let Some(doc) = ours {
            if should_yield(&doc, *their_workspace, *their_size) {
                info!(machine = %machine_id, "adopting the other machine's configuration");
                if let Err(error) = agent
                    .adopt_configuration_of(*machine_id, *identity_key)
                    .await
                {
                    warn!(%error, "could not adopt the other configuration");
                }
            } else {
                debug!(machine = %machine_id, "keeping this configuration; they will adopt it");
            }
        }
        return;
    }

    let core = &agent.core;
    let mut state = core.state.lock().expect("agent state");
    match event {
        NetEvent::Online { machine_id } => {
            state.online.insert(machine_id);
            state.candidates.remove(&machine_id);
        }
        NetEvent::Offline { machine_id, .. } => {
            state.online.remove(&machine_id);
        }
        NetEvent::Candidate {
            machine_id,
            display_name,
            identity_key,
            workspace_id,
            addr,
            invited_us,
        } => {
            state.candidates.insert(
                machine_id,
                Candidate {
                    machine_id,
                    display_name,
                    identity_key,
                    invited_us,
                    addr: addr.to_string(),
                    last_seen_ms: is_core::now_ms(),
                    has_workspace: workspace_id.is_some(),
                },
            );
        }
        NetEvent::Rejected {
            machine_id,
            identity_key,
            addr,
        } => {
            // Worth surfacing rather than burying: from the user's side this is
            // "the other computer has not paired me back yet".
            state.candidates.entry(machine_id).or_insert(Candidate {
                machine_id,
                display_name: "Unknown computer".into(),
                identity_key,
                invited_us: false,
                addr: addr.to_string(),
                last_seen_ms: is_core::now_ms(),
                has_workspace: true,
            });
        }
        // The merge already persisted and announced; nothing to add here.
        NetEvent::WorkspaceChanged | NetEvent::DifferentWorkspaces { .. } => return,
        NetEvent::Sighted { .. } | NetEvent::Connecting { .. } => return,
    }
    drop(state);
    core.announce_change();
}
