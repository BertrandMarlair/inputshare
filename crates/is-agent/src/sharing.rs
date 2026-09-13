//! Actually sharing the keyboard and mouse.
//!
//! One rule, and everything else follows from it:
//!
//! > **Exactly one machine owns the pointer, and every machine knows which.**
//!
//! - The owner moves its own pointer normally. Its keyboard types where it is.
//! - Every other machine swallows its local input and sends it to the owner
//!   instead, so *any* mouse and *any* keyboard drives the one pointer.
//! - Ownership only changes by announcement, never by a machine deciding on its
//!   own.
//!
//! That last point is the one that was missing, and it is why the pointer used
//! to get stuck. Each machine tracked the cursor privately, so after a crossing
//! both believed they held it: one stayed deaf to its own keyboard while the
//! other happily used it, and the only way out was to walk the pointer back with
//! the mouse that appeared dead.
//!
//! The other half of the same bug: input arriving from a peer was injected
//! straight into the desktop without passing through the routing below. The
//! owner therefore never saw those movements as movements, so a pointer being
//! driven by the *other* machine's mouse could never cross back. Remote input
//! now goes through exactly the same path as local input.
//!
//! ## Why this file is careful
//!
//! While the pointer is on another machine, this process swallows every
//! keystroke on this one. If the loop stops — a panic, a deadlock, a channel
//! nobody drains — the computer stops responding to its own keyboard, and the
//! user may not be able to click the thing that would fix it.
//!
//! So the heartbeat below is not bookkeeping. It is what gives the keyboard back
//! if this loop dies, and it is sent from inside the loop that would stop
//! sending it. On top of that the hook releases on Ctrl+Alt+F12 without asking
//! anything here. See `is_input::capture`.

use std::sync::Arc;
use std::time::Duration;

use is_core::layout::resolve_cursor;
use is_core::{InputEvent, MachineId, Point, Resolution};
use is_input::capture::Capture;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{Agent, Error, Result};

/// How often the loop tells the capture hook it is still alive. Comfortably
/// under the hook's own limit, so a slow moment is not mistaken for a crash.
const HEARTBEAT: Duration = Duration::from_millis(400);

/// Motion is coalesced for this long before being sent. Enough to turn a burst
/// of mouse samples into one frame, short enough that nobody feels it.
const BATCH: Duration = Duration::from_millis(4);

/// How far the tracked pointer may drift from the real one before it is
/// resynchronised. Something else moved it — a window taking focus, a game — and
/// the real one is right.
const DRIFT_LIMIT: i32 = 80;

/// Where an event came from. The routing is identical; only what happens to the
/// pointer afterwards differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// This machine's own keyboard and mouse.
    Local,
    /// Another machine's, arriving over the network.
    Remote,
}

/// Stops capture and gives the local keyboard back.
///
/// Prefer [`Sharing::stop`] over dropping it: stopping has to finish before
/// sharing can start again, because the hooks are a process-wide resource.
pub struct Sharing {
    stop: Arc<tokio::sync::Notify>,
    stopped: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl Sharing {
    /// Stops capture and waits for the hooks to actually be gone.
    pub async fn stop(mut self) {
        // `notify_one`, not `notify_waiters`: the latter only wakes tasks parked
        // at that instant, and this loop is also woken by timers. A stop landing
        // between ticks would simply be lost, and the hooks would stay in.
        self.stop.notify_one();
        if let Some(stopped) = self.stopped.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), stopped).await;
        }
    }
}

impl Drop for Sharing {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

/// Starts capturing and routing input. Fails if the platform cannot capture.
pub fn start(agent: Arc<Agent>) -> Result<Sharing> {
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<InputEvent>();
    let capture = Capture::start(raw_tx).map_err(|error| Error::Refused(error.to_string()))?;

    // The hook thread speaks blocking std channels; bridge once, here, rather
    // than making the hook care about async.
    let (local_tx, local_rx) = mpsc::channel::<InputEvent>(4096);
    std::thread::Builder::new()
        .name("inputshare-bridge".into())
        .spawn(move || {
            while let Ok(event) = raw_rx.recv() {
                if local_tx.blocking_send(event).is_err() {
                    break;
                }
            }
        })
        .map_err(|error| Error::Refused(error.to_string()))?;

    // Input arriving from peers is routed rather than injected blindly, so the
    // owner can notice that somebody else's mouse is pushing towards an edge.
    let (remote_tx, remote_rx) = mpsc::channel::<(MachineId, Vec<InputEvent>)>(1024);
    agent.set_remote_input_sink(Some(remote_tx));

    let stop = Arc::new(tokio::sync::Notify::new());
    let (finished, stopped) = tokio::sync::oneshot::channel();
    tokio::spawn(run(
        agent,
        capture,
        local_rx,
        remote_rx,
        stop.clone(),
        finished,
    ));
    Ok(Sharing {
        stop,
        stopped: Some(stopped),
    })
}

struct Router {
    agent: Arc<Agent>,
    local: MachineId,
    /// The machine that owns the pointer. Agreed by everybody.
    owner: MachineId,
    /// Where the pointer is, in workspace coordinates. Only meaningful on the
    /// owner; every other machine simply forwards.
    cursor: Point,
    /// Events waiting to go to the owner.
    pending: Vec<InputEvent>,
}

async fn run(
    agent: Arc<Agent>,
    capture: Capture,
    mut local: mpsc::Receiver<InputEvent>,
    mut remote: mpsc::Receiver<(MachineId, Vec<InputEvent>)>,
    stop: Arc<tokio::sync::Notify>,
    finished: tokio::sync::oneshot::Sender<()>,
) {
    let me = agent.identity().machine_id;
    let mut router = Router {
        local: me,
        owner: me,
        cursor: pointer_now(&agent).unwrap_or_default(),
        pending: Vec::new(),
        agent,
    };
    router.agent.note_cursor_owner(Some(me));

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut flush = tokio::time::interval(BATCH);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!("input sharing started; the pointer is on this machine");

    loop {
        tokio::select! {
            _ = stop.notified() => break,
            _ = heartbeat.tick() => {
                capture.heartbeat();
                // The machine holding the pointer went away. Take it, and say
                // so — otherwise it is stranded somewhere unreachable and this
                // keyboard stays swallowed for nothing.
                if router.owner != me && !router.owner_is_reachable() {
                    warn!("the machine holding the pointer went offline; taking it back");
                    router.take_ownership(&capture).await;
                }
            }
            _ = flush.tick() => {
                router.apply_announcement(&capture).await;
                router.flush().await;
            }
            event = local.recv() => match event {
                Some(event) => router.handle(event, Source::Local, &capture).await,
                None => break,
            },
            batch = remote.recv() => match batch {
                Some((from, events)) => {
                    for event in events {
                        router.handle(event, Source::Remote, &capture).await;
                    }
                    let _ = from;
                }
                None => break,
            },
        }
    }

    capture.set_suppressing(false);
    // Dropping removes the hooks and joins the hook thread, so by the time this
    // returns nothing is capturing any more.
    drop(capture);
    router.agent.set_remote_input_sink(None);
    router.agent.note_cursor_owner(None);
    info!("input sharing stopped; local keyboard and mouse restored");
    let _ = finished.send(());
}

/// Where this machine's pointer is, in workspace coordinates.
fn pointer_now(agent: &Arc<Agent>) -> Option<Point> {
    let view = agent.view();
    let record = view.doc?.machine(view.identity.machine_id).cloned()?;
    let (x, y) = is_input::inject::cursor_position().ok()?;
    Some(Point::new(x + record.position.x, y + record.position.y))
}

impl Router {
    fn owner_is_reachable(&self) -> bool {
        self.agent.view().online.contains(&self.owner)
    }

    /// Applies an ownership announcement from another machine.
    async fn apply_announcement(&mut self, capture: &Capture) {
        let Some(owner) = self.agent.take_announced_owner() else {
            return;
        };
        if owner == self.owner {
            return;
        }
        debug!(%owner, "the pointer moved");
        self.owner = owner;
        self.agent.note_cursor_owner(Some(owner));

        if owner == self.local {
            capture.set_suppressing(false);
            // Only trust the real pointer if the handoff did not already place
            // it: the entry point is the accurate answer, this is the fallback.
            if let Some(here) = pointer_now(&self.agent) {
                self.cursor = here;
            }
        } else {
            self.pending.clear();
            capture.set_suppressing(true);
        }
    }

    /// Takes the pointer for this machine and tells everyone.
    async fn take_ownership(&mut self, capture: &Capture) {
        self.owner = self.local;
        self.pending.clear();
        capture.set_suppressing(false);
        if let Some(here) = pointer_now(&self.agent) {
            self.cursor = here;
        }
        self.agent.note_cursor_owner(Some(self.local));
        self.agent.announce_cursor_owner(self.local).await;
    }

    async fn handle(&mut self, event: InputEvent, source: Source, capture: &Capture) {
        // The handoff itself, and it has to be handled before anything asks who
        // the owner is — because at this instant it is still the machine that
        // sent it. A machine only sends this to the one it is handing the
        // pointer to, so receiving it *is* becoming the owner.
        //
        // Dropping it, as this used to, left the pointer wherever it happened to
        // be sitting: cross downwards, come back upwards, and it reappeared at
        // the bottom.
        if let (InputEvent::CursorEnter { x, y }, Source::Remote) = (event, source) {
            if self.owner != self.local {
                info!("the pointer was handed to this machine");
                self.owner = self.local;
                self.pending.clear();
                capture.set_suppressing(false);
                self.agent.note_cursor_owner(Some(self.local));
            }
            let _ = is_input::inject::set_cursor_position(x, y);
            if let Some(here) = pointer_now(&self.agent) {
                self.cursor = here;
            }
            return;
        }

        // Not the owner: this machine is a keyboard and a mouse, nothing more.
        // Everything it sees goes to whoever has the pointer.
        if self.owner != self.local {
            if source == Source::Local {
                self.pending.push(event);
            }
            return;
        }

        match event {
            InputEvent::MouseMove { dx, dy } => self.on_motion(dx, dy, source, capture).await,
            // Already handled above when it came from a peer; one produced
            // locally means nothing.
            InputEvent::CursorEnter { .. } => {}
            // Local keys and clicks already acted on this machine; remote ones
            // have to be replayed.
            other => {
                if source == Source::Remote {
                    let _ = is_input::inject::inject(other);
                }
            }
        }
    }

    async fn on_motion(&mut self, dx: i32, dy: i32, source: Source, capture: &Capture) {
        let view = self.agent.view();
        let Some(doc) = view.doc.as_ref() else {
            return;
        };
        let mut online = view.online.clone();
        online.insert(self.local);

        // Something else may have moved the pointer — a window taking focus, a
        // game warping it. The real one wins when they disagree by more than a
        // nudge.
        if let Some(here) = pointer_now(&self.agent) {
            if (here.x - self.cursor.x).abs() > DRIFT_LIMIT
                || (here.y - self.cursor.y).abs() > DRIFT_LIMIT
            {
                self.cursor = here;
            }
        }

        let target = Point::new(self.cursor.x + dx, self.cursor.y + dy);
        match resolve_cursor(doc, &online, self.local, target) {
            Resolution::Stay(point) => {
                self.cursor = point;
                // Local movement already moved the pointer; remote movement has
                // to be applied. Placing it outright rather than replaying the
                // delta keeps pointer acceleration out of it.
                if source == Source::Remote {
                    if let Some(record) = doc.machine(self.local) {
                        let _ = is_input::inject::set_cursor_position(
                            point.x - record.position.x,
                            point.y - record.position.y,
                        );
                    }
                }
            }
            Resolution::Move { machine, point } => {
                self.cursor = point;
                self.owner = machine;
                self.agent.note_cursor_owner(Some(machine));

                // Tell the receiving machine where the pointer lands, before any
                // movement reaches it.
                if let Some(record) = doc.machine(machine) {
                    self.pending.push(InputEvent::CursorEnter {
                        x: point.x - record.position.x,
                        y: point.y - record.position.y,
                    });
                }
                capture.set_suppressing(true);
                info!(to = %machine, "pointer crossed to another machine");

                self.flush().await;
                self.agent.announce_cursor_owner(machine).await;
            }
        }
    }

    async fn flush(&mut self) {
        if self.pending.is_empty() || self.owner == self.local {
            self.pending.clear();
            return;
        }
        let events = std::mem::take(&mut self.pending);
        let owner = self.owner;
        self.agent.send_input(owner, events).await;
    }
}
