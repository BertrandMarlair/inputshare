//! Two machines, one network. Requirements 5, 6, 9 and 10, end to end.
//!
//! These run three real services over real multicast on the loopback interface,
//! which is as close to "two computers" as one machine gets. They are slower
//! than unit tests on purpose: discovery is a timing problem, and a test that
//! stubs the timing out would not catch the thing most likely to break.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use is_core::{
    DisplayInfo, Identity, InputAssignment, InputEvent, MachineId, MachineRecord, MouseButton,
    Platform, Point, WorkspaceDoc, WorkspaceUpdate,
};
use is_net::{Config, Event, Service, Workspace};
use tokio::sync::broadcast;

/// An in-memory stand-in for the agent's store.
struct Harness {
    name: String,
    doc: Mutex<Option<WorkspaceDoc>>,
    /// Input replayed here, recorded instead of injected. Injection is the one
    /// step that has to touch the real desktop; everything up to it can be
    /// checked without moving anybody's mouse.
    received_input: Mutex<Vec<(MachineId, Vec<InputEvent>)>>,
    /// Ownership announcements, as (sender, owner).
    owner_announcements: Mutex<Vec<(MachineId, MachineId)>>,
}

impl Harness {
    fn new(name: &str, doc: WorkspaceDoc) -> Self {
        Self {
            name: name.into(),
            doc: Mutex::new(Some(doc)),
            received_input: Mutex::new(Vec::new()),
            owner_announcements: Mutex::new(Vec::new()),
        }
    }

    fn empty(name: &str) -> Self {
        Self {
            name: name.into(),
            doc: Mutex::new(None),
            received_input: Mutex::new(Vec::new()),
            owner_announcements: Mutex::new(Vec::new()),
        }
    }

    fn workspace_name(&self) -> Option<String> {
        self.doc
            .lock()
            .unwrap()
            .as_ref()
            .map(|d| d.name.value.clone())
    }
}

impl Workspace for Harness {
    fn snapshot(&self) -> Option<WorkspaceDoc> {
        self.doc.lock().unwrap().clone()
    }

    fn merge(&self, incoming: &WorkspaceDoc) -> is_core::Result<bool> {
        let mut guard = self.doc.lock().unwrap();
        match guard.as_mut() {
            Some(doc) => doc.merge(incoming),
            None => {
                *guard = Some(incoming.clone());
                Ok(true)
            }
        }
    }

    fn apply(&self, update: WorkspaceUpdate) -> is_core::Result<bool> {
        let mut guard = self.doc.lock().unwrap();
        match guard.as_mut() {
            Some(doc) => doc.apply(update),
            None => Ok(false),
        }
    }

    fn display_name(&self) -> String {
        self.name.clone()
    }

    fn on_cursor_owner(&self, from: MachineId, machine: MachineId) {
        self.owner_announcements
            .lock()
            .unwrap()
            .push((from, machine));
    }

    fn on_input(&self, from: MachineId, events: Vec<InputEvent>) {
        self.received_input.lock().unwrap().push((from, events));
    }
}

fn record(identity: &Identity, name: &str, x: i32) -> MachineRecord {
    MachineRecord {
        id: identity.machine_id,
        display_name: name.into(),
        platform: Platform::current(),
        public_key: identity.public_key(),
        position: Point::new(x, 0),
        displays: vec![DisplayInfo {
            id: "display-0".into(),
            name: "Primary".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }],
        input: InputAssignment::default(),
        paired_at_ms: 0,
    }
}

/// A port nothing else is using, shared by every service in the test — which is
/// how multicast works anyway: every member binds the same port.
fn scratch_port() -> u16 {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .expect("a free udp port")
        .local_addr()
        .expect("bound address")
        .port()
}

fn config(identity: Identity, discovery_port: u16) -> Config {
    Config {
        identity,
        discovery_port,
        // Ephemeral, so three services can coexist on one host.
        transport_port: 0,
        announce_interval: Duration::from_millis(250),
    }
}

/// Waits for an event matching `predicate`, or gives up.
async fn wait_for(
    events: &mut broadcast::Receiver<Event>,
    within: Duration,
    predicate: impl Fn(&Event) -> bool,
) -> Option<Event> {
    tokio::time::timeout(within, async {
        loop {
            match events.recv().await {
                Ok(event) if predicate(&event) => return Some(event),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paired_machines_find_each_other_and_converge() {
    let alice = Identity::generate();
    let bob = Identity::generate();

    // Both machines were paired before and have just booted: each already holds
    // the other's record, including its public key.
    let mut doc = WorkspaceDoc::create("Workshop", record(&alice, "Alice", 0));
    doc.upsert_machine(alice.machine_id, record(&bob, "Bob", 1920));

    let alice_ws = Arc::new(Harness::new("Alice", doc.clone()));
    let bob_ws = Arc::new(Harness::new("Bob", doc.clone()));

    let port = scratch_port();
    let alice_net = Service::start(config(alice.clone(), port), alice_ws.clone())
        .await
        .expect("alice starts");
    let mut alice_events = alice_net.subscribe();

    let bob_net = Service::start(config(bob.clone(), port), bob_ws.clone())
        .await
        .expect("bob starts");
    let mut bob_events = bob_net.subscribe();

    // Requirements 5 and 6: nobody typed an address, and nobody pressed connect.
    let online = Duration::from_secs(25);
    let alice_sees = wait_for(
        &mut alice_events,
        online,
        |event| matches!(event, Event::Online { machine_id } if *machine_id == bob.machine_id),
    )
    .await;
    assert!(
        alice_sees.is_some(),
        "Alice never saw Bob come online by herself"
    );

    let bob_sees = wait_for(
        &mut bob_events,
        online,
        |event| matches!(event, Event::Online { machine_id } if *machine_id == alice.machine_id),
    )
    .await;
    assert!(bob_sees.is_some(), "Bob never saw Alice come online");

    // Requirement 10: a change on one machine reaches the other while both are up.
    let update = {
        let mut guard = alice_ws.doc.lock().unwrap();
        let doc = guard.as_mut().unwrap();
        doc.set_name(alice.machine_id, "Studio")
    };
    alice_net.broadcast(update).await;

    let changed = wait_for(&mut bob_events, Duration::from_secs(10), |event| {
        matches!(event, Event::WorkspaceChanged)
    })
    .await;
    assert!(
        changed.is_some(),
        "Bob was never told the workspace changed"
    );
    assert_eq!(
        bob_ws.workspace_name().as_deref(),
        Some("Studio"),
        "the rename did not reach Bob"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpaired_machine_is_offered_for_pairing_and_nothing_more() {
    let alice = Identity::generate();
    let stranger = Identity::generate();

    // Alice's workspace does not contain the stranger.
    let doc = WorkspaceDoc::create("Workshop", record(&alice, "Alice", 0));
    let alice_ws = Arc::new(Harness::new("Alice", doc));
    let stranger_ws = Arc::new(Harness::empty("Stranger"));

    let port = scratch_port();
    let alice_net = Service::start(config(alice.clone(), port), alice_ws.clone())
        .await
        .expect("alice starts");
    let mut alice_events = alice_net.subscribe();

    let stranger_net = Service::start(config(stranger.clone(), port), stranger_ws.clone())
        .await
        .expect("stranger starts");
    let mut stranger_events = stranger_net.subscribe();

    // Requirement 15: it shows up as something the user could pair with.
    let offered = wait_for(&mut alice_events, Duration::from_secs(20), |event| {
        matches!(event, Event::Candidate { machine_id, .. } if *machine_id == stranger.machine_id)
    })
    .await;
    assert!(
        offered.is_some(),
        "an unknown machine on the network should be offered for pairing"
    );

    // And that is all it gets. No session, and nothing it sends can touch the
    // workspace.
    let connected = wait_for(&mut stranger_events, Duration::from_secs(6), |event| {
        matches!(event, Event::Online { .. })
    })
    .await;
    assert!(
        connected.is_none(),
        "an unpaired machine must not reach an online session"
    );
    assert_eq!(
        alice_ws.workspace_name().as_deref(),
        Some("Workshop"),
        "an unpaired machine must not be able to change the workspace"
    );
}

/// Keyboard and mouse events have to survive the trip: framed, encrypted,
/// decrypted and handed over unchanged, to the right machine.
///
/// This covers everything between the two desktops. Capture and injection touch
/// the real hardware and are exercised separately; what is checked here is that
/// nothing in the middle mangles or misroutes an event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_events_cross_the_link_intact() {
    let alice = Identity::generate();
    let bob = Identity::generate();

    let mut doc = WorkspaceDoc::create("Workshop", record(&alice, "Alice", 0));
    doc.upsert_machine(alice.machine_id, record(&bob, "Bob", 1920));

    let alice_ws = Arc::new(Harness::new("Alice", doc.clone()));
    let bob_ws = Arc::new(Harness::new("Bob", doc));

    let port = scratch_port();
    let alice_net = Service::start(config(alice.clone(), port), alice_ws.clone())
        .await
        .expect("alice starts");
    let mut alice_events = alice_net.subscribe();
    // Kept alive for the test; Bob's side is driven entirely by what arrives.
    let _bob_net = Service::start(config(bob.clone(), port), bob_ws.clone())
        .await
        .expect("bob starts");

    assert!(
        wait_for(&mut alice_events, Duration::from_secs(25), |event| {
            matches!(event, Event::Online { machine_id } if *machine_id == bob.machine_id)
        })
        .await
        .is_some(),
        "they never connected"
    );

    // What one machine captures when the pointer has crossed over.
    let sent = vec![
        InputEvent::CursorEnter { x: 12, y: 340 },
        InputEvent::MouseMove { dx: -7, dy: 3 },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
        },
        InputEvent::Key {
            vk: 0x41,
            scan: 0x1e,
            down: true,
            extended: false,
        },
        InputEvent::Wheel {
            delta: -120,
            horizontal: false,
        },
    ];
    alice_net.send_input(bob.machine_id, sent.clone()).await;

    let arrived = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some((from, events)) = bob_ws.received_input.lock().unwrap().first().cloned() {
                return (from, events);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    let (from, events) = arrived.expect("input never arrived on the other machine");
    assert_eq!(
        from, alice.machine_id,
        "it must be attributed to the sender"
    );
    assert_eq!(
        events, sent,
        "every event must arrive exactly as it was sent"
    );

    // And nothing must leak back the other way.
    assert!(
        alice_ws.received_input.lock().unwrap().is_empty(),
        "input must not echo back to the machine that sent it"
    );
}

/// Every machine has to be told where the pointer is.
///
/// Ownership used to be a private opinion on each machine, so after a crossing
/// both believed they held the pointer: one stayed deaf to its own keyboard
/// while the other used it, and the only escape was walking the pointer back
/// with the mouse that appeared dead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_machine_holding_the_pointer_is_announced_to_everyone() {
    let alice = Identity::generate();
    let bob = Identity::generate();

    let mut doc = WorkspaceDoc::create("Workshop", record(&alice, "Alice", 0));
    doc.upsert_machine(alice.machine_id, record(&bob, "Bob", 1920));

    let alice_ws = Arc::new(Harness::new("Alice", doc.clone()));
    let bob_ws = Arc::new(Harness::new("Bob", doc));

    let port = scratch_port();
    let alice_net = Service::start(config(alice.clone(), port), alice_ws.clone())
        .await
        .expect("alice starts");
    let mut alice_events = alice_net.subscribe();
    let _bob_net = Service::start(config(bob.clone(), port), bob_ws.clone())
        .await
        .expect("bob starts");

    assert!(
        wait_for(&mut alice_events, Duration::from_secs(25), |event| {
            matches!(event, Event::Online { machine_id } if *machine_id == bob.machine_id)
        })
        .await
        .is_some(),
        "they never connected"
    );

    // Alice hands the pointer to Bob and says so.
    alice_net.announce_cursor_owner(bob.machine_id).await;

    let heard = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(entry) = bob_ws.owner_announcements.lock().unwrap().first().copied() {
                return entry;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the announcement never arrived");

    assert_eq!(heard.0, alice.machine_id, "it has to say who sent it");
    assert_eq!(
        heard.1, bob.machine_id,
        "and which machine now has the pointer"
    );
    assert!(
        alice_ws.owner_announcements.lock().unwrap().is_empty(),
        "a machine must not hear its own announcement"
    );
}
