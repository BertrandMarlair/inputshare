//! Two machines, two config directories, one network.
//!
//! Everything here goes through the real agent, the real files on disk and a
//! real multicast socket. A test that stubbed any of those would pass while the
//! product stayed broken — which is exactly what kept happening before these
//! existed.

use std::time::Duration;

use is_agent::{Agent, AgentEvent};
use is_core::{LocalPrefs, Store};
use tokio::sync::broadcast;

/// A port nothing else on this machine is using, so a running copy of the app
/// cannot make the tests flaky.
fn scratch_port() -> u16 {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .expect("a free udp port")
        .local_addr()
        .expect("bound address")
        .port()
}

/// Waits for a condition to hold, or gives up.
///
/// Polls rather than waiting on the change stream. Waiting on events looks
/// tidier but is a trap: if the final transition produces the last event, or two
/// coalesce, the check never runs again and the test fails on a timeout that has
/// nothing to do with the behaviour.
async fn settle(
    _events: &mut broadcast::Receiver<AgentEvent>,
    within: Duration,
    mut done: impl FnMut() -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if done() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn agent_named(dir: &std::path::Path, name: &str) -> std::sync::Arc<Agent> {
    let agent = Agent::load(Store::at(dir).expect("store")).expect("agent loads");
    agent.rename_this_machine(name).await.expect("rename");
    agent
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_machine_is_usable_without_being_set_up() {
    let dir = tempfile::tempdir().expect("temp dir");
    let agent = Agent::load(Store::at(dir.path()).expect("store")).expect("agent loads");

    // No setup step. A computer on its own is already a complete configuration
    // of one machine, and it knows its own screens.
    let view = agent.view();
    let doc = view
        .doc
        .expect("a fresh machine already has a configuration");
    let me = doc
        .machine(view.identity.machine_id)
        .expect("it contains this machine");
    assert!(!me.display_name.is_empty(), "it has a name to show");
    assert!(
        me.bounds().is_some_and(|b| b.width > 0),
        "it has somewhere for a cursor to be"
    );

    // And it is on disk, so the next launch is not a first launch.
    assert!(Store::at(dir.path())
        .expect("store")
        .load_workspace()
        .expect("readable")
        .is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_computers_pair_and_end_up_in_one_configuration() {
    let alice_dir = tempfile::tempdir().expect("temp dir");
    let bob_dir = tempfile::tempdir().expect("temp dir");
    let port = scratch_port();

    let alice = agent_named(alice_dir.path(), "Alice").await;
    let bob = agent_named(bob_dir.path(), "Bob").await;

    // Each starts with its own configuration, because each is a computer that
    // was switched on. They are not the same configuration, and that is exactly
    // the situation that used to leave two paired machines offline forever.
    assert_ne!(
        alice.view().doc.unwrap().workspace_id,
        bob.view().doc.unwrap().workspace_id
    );

    alice.go_online_on(port, 0).await.expect("alice online");
    bob.go_online_on(port, 0).await.expect("bob online");

    let mut alice_events = alice.subscribe();
    let mut bob_events = bob.subscribe();
    let alice_id = alice.identity().machine_id;
    let bob_id = bob.identity().machine_id;

    assert!(
        settle(&mut alice_events, Duration::from_secs(25), || alice
            .view()
            .candidates
            .iter()
            .any(|c| c.machine_id == bob_id))
        .await,
        "Alice never discovered Bob"
    );

    // Alice admits Bob. Bob has to be told, or one person clicks Pair and the
    // other has nothing on screen to act on.
    alice.pair(bob_id).await.expect("alice admits bob");
    assert!(
        settle(&mut bob_events, Duration::from_secs(25), || bob
            .view()
            .candidates
            .iter()
            .any(|c| c.machine_id == alice_id && c.invited_us))
        .await,
        "Bob was admitted but never told, so pairing looks like it did nothing"
    );
    bob.pair(alice_id).await.expect("bob accepts");

    // From here nobody touches anything. Two configurations cannot merge, so one
    // gives way — decided by both sides from the same numbers, with no question
    // put to anybody.
    let together = settle(&mut bob_events, Duration::from_secs(40), || {
        let a = alice.view();
        let b = bob.view();
        a.online.contains(&bob_id)
            && b.online.contains(&alice_id)
            && a.doc.map(|d| d.workspace_id) == b.doc.map(|d| d.workspace_id)
    })
    .await;
    assert!(
        together,
        "two computers that pair must end up in one configuration by themselves"
    );

    // And both must have a size, or there is nothing to place on the canvas and
    // nowhere to route a cursor.
    let doc = alice.view().doc.expect("configuration");
    assert_eq!(doc.machines_present().count(), 2);
    for machine in doc.machines_present() {
        assert!(
            machine
                .bounds()
                .is_some_and(|b| b.width > 0 && b.height > 0),
            "{} has no screens, so it cannot be placed",
            machine.display_name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_nobody_admitted_cannot_join() {
    let alice_dir = tempfile::tempdir().expect("temp dir");
    let stranger_dir = tempfile::tempdir().expect("temp dir");
    let port = scratch_port();

    let alice = agent_named(alice_dir.path(), "Alice").await;
    let stranger = agent_named(stranger_dir.path(), "Stranger").await;

    alice.go_online_on(port, 0).await.expect("alice online");
    stranger
        .go_online_on(port, 0)
        .await
        .expect("stranger online");

    let mut stranger_events = stranger.subscribe();
    let alice_id = alice.identity().machine_id;
    let alice_workspace = alice.view().doc.unwrap().workspace_id;

    assert!(
        settle(&mut stranger_events, Duration::from_secs(25), || stranger
            .view()
            .candidates
            .iter()
            .any(|c| c.machine_id == alice_id))
        .await,
        "the stranger never discovered Alice"
    );

    // The stranger asks to join. Alice never admits it.
    stranger.pair(alice_id).await.expect("stranger asks");

    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(
        !stranger.view().online.contains(&alice_id),
        "wanting in is not being in"
    );
    assert_eq!(
        alice.view().doc.map(|d| d.machines_present().count()),
        Some(1),
        "Alice's configuration must still be hers alone"
    );
    assert_eq!(
        alice.view().doc.map(|d| d.workspace_id),
        Some(alice_workspace),
        "and it must not have been replaced by anyone else's"
    );
}

/// Turning sharing on must not take the process with it.
///
/// It used to: starting it spawns the routing task, and spawning outside a
/// runtime panics. The switch was a synchronous call from the window, so the
/// application died the instant anybody ticked the box.
///
/// Capture starts in observe-only mode, so this installs the hooks and swallows
/// nothing — running it cannot leave a machine unable to use its own keyboard.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(windows)]
async fn turning_sharing_on_and_off_is_survivable() {
    let dir = tempfile::tempdir().expect("temp dir");
    let agent = agent_named(dir.path(), "Solo").await;

    assert!(!agent.is_sharing(), "sharing is off until asked");

    agent.set_sharing(true).await.expect("sharing starts");
    assert!(agent.is_sharing());
    assert!(
        agent.sharing_was_on(),
        "the choice has to be on disk, or the next boot starts with it off"
    );

    // Idempotent: ticking a box twice must not start a second capture.
    agent.set_sharing(true).await.expect("already on");

    agent.set_sharing(false).await.expect("sharing stops");
    assert!(!agent.is_sharing());

    assert!(!agent.sharing_was_on(), "switched off means switched off");

    // And it can come back, which is what a person toggling the box does.
    agent.set_sharing(true).await.expect("restarts");
    agent.set_sharing(false).await.expect("stops again");
}

/// A machine that was sharing when it was shut down comes back sharing.
///
/// No hooks are installed here. Input capture is one process-wide resource, so
/// a test that started it would be racing the test above rather than checking
/// anything about restarts; what matters is that the choice on disk is what a
/// fresh agent reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reboot_remembers_that_this_machine_was_sharing() {
    let dir = tempfile::tempdir().expect("temp dir");

    let agent = agent_named(dir.path(), "Desk").await;
    assert!(!agent.sharing_was_on(), "off until somebody asks");
    drop(agent);

    // What switching the box on writes down.
    Store::at(dir.path())
        .expect("store")
        .save_prefs(&LocalPrefs { sharing: true })
        .expect("saved");

    let after_reboot = agent_named(dir.path(), "Desk").await;
    assert!(
        after_reboot.sharing_was_on(),
        "a reboot must not silently stop sharing this machine's keyboard"
    );
    assert!(
        !after_reboot.is_sharing(),
        "loading must not install hooks on its own; the app asks once it is online"
    );
}
