//! Requirements 2 and 12, and the critical requirement: a reboot must never
//! reset the workspace.

mod common;

use std::fs;

use common::machine;
use is_core::{LocalPrefs, Point, Store, WorkspaceDoc};

fn new_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::at(dir.path()).expect("store opens");
    (dir, store)
}

#[test]
fn machine_identity_is_minted_once_and_then_stable() {
    let (_dir, store) = new_store();

    let first = store.load_or_create_identity().expect("identity created");
    let second = store.load_or_create_identity().expect("identity loaded");

    assert_eq!(
        first.machine_id, second.machine_id,
        "a second launch must not mint a new machine ID"
    );
    assert_eq!(first.public_key(), second.public_key());
}

#[test]
fn identity_signatures_verify_across_a_reload() {
    let (_dir, store) = new_store();

    let identity = store.load_or_create_identity().expect("identity");
    let signature = identity.sign(b"pairing challenge");

    let reloaded = store.load_or_create_identity().expect("identity");
    assert!(reloaded
        .public_key()
        .verify(b"pairing challenge", &signature));
    assert!(!reloaded.public_key().verify(b"different bytes", &signature));
}

#[test]
fn workspace_survives_a_reboot() {
    let (dir, store) = new_store();

    let pc1 = machine("PC1", Point::new(0, 0));
    let pc2 = machine("PC2", Point::new(0, -1080));
    let mut doc = WorkspaceDoc::create("My Desk", pc1.clone());
    doc.upsert_machine(pc1.id, pc2.clone());
    store.save_workspace(&doc).expect("saved");

    // Everything shuts down. A fresh process opens the same directory.
    let rebooted = Store::at(dir.path()).expect("store opens");
    let loaded = rebooted
        .load_workspace()
        .expect("readable")
        .expect("a workspace is present");

    assert_eq!(loaded.workspace_id, doc.workspace_id);
    assert_eq!(loaded.name.value, "My Desk");
    assert_eq!(loaded.machines_present().count(), 2);
    assert_eq!(
        loaded.machine(pc2.id).map(|m| m.position),
        Some(Point::new(0, -1080)),
        "the configured topology comes back, not a default"
    );
}

#[test]
fn a_machine_with_no_workspace_reports_none_rather_than_failing() {
    let (_dir, store) = new_store();
    assert!(store.load_workspace().expect("readable").is_none());
}

#[test]
fn a_truncated_workspace_file_falls_back_to_the_previous_copy() {
    let (_dir, store) = new_store();

    let pc1 = machine("PC1", Point::new(0, 0));
    let mut doc = WorkspaceDoc::create("My Desk", pc1.clone());
    store.save_workspace(&doc).expect("first save");
    doc.set_name(pc1.id, "Studio");
    store
        .save_workspace(&doc)
        .expect("second save keeps a .bak");

    // Power loss mid-write.
    fs::write(store.dir().join("workspace.json"), b"{\"workspace_id\":").expect("truncate");

    let recovered = store
        .load_workspace()
        .expect("recovers from .bak")
        .expect("a workspace is present");

    assert_eq!(
        recovered.name.value, "My Desk",
        "losing the last write is acceptable; losing the workspace is not"
    );
}

#[test]
fn a_corrupt_file_with_no_backup_is_an_error_not_a_silent_reset() {
    let (_dir, store) = new_store();
    fs::write(store.dir().join("workspace.json"), b"not json at all").expect("write");

    assert!(
        store.load_workspace().is_err(),
        "reporting None here would look like a first launch and re-run onboarding"
    );
}

#[test]
fn a_future_schema_version_is_refused() {
    let (_dir, store) = new_store();
    let pc1 = machine("PC1", Point::new(0, 0));
    let mut doc = WorkspaceDoc::create("My Desk", pc1);
    doc.schema_version = WorkspaceDoc::SCHEMA_VERSION + 1;
    store.save_workspace(&doc).expect("saved");

    assert!(
        store.load_workspace().is_err(),
        "an older build must not drop fields it does not understand"
    );
}

#[test]
fn peer_hints_are_optional_and_default_to_empty() {
    let (_dir, store) = new_store();
    assert!(store.load_hints().expect("readable").peers.is_empty());
}

#[test]
fn a_backup_restores_identity_and_workspace() {
    let (dir, store) = new_store();

    let identity = store.load_or_create_identity().expect("identity");
    let pc1 = machine("PC1", Point::new(0, 0));
    let doc = WorkspaceDoc::create("My Desk", pc1);
    store.save_workspace(&doc).expect("saved");

    let backup_path = dir.path().join("backup.json");
    store.export_backup(&backup_path).expect("exported");

    // A reinstall: fresh config directory, which by default is a new machine.
    let (_fresh_dir, fresh) = new_store();
    let new_machine = fresh.load_or_create_identity().expect("identity");
    assert_ne!(
        new_machine.machine_id, identity.machine_id,
        "a reinstall is a new machine unless the user restores a backup"
    );

    let restored = fresh.import_backup(&backup_path).expect("imported");
    assert_eq!(restored.machine_id, identity.machine_id);
    assert_eq!(
        fresh
            .load_or_create_identity()
            .expect("identity")
            .machine_id,
        identity.machine_id
    );
    assert_eq!(
        fresh
            .load_workspace()
            .expect("readable")
            .expect("present")
            .workspace_id,
        doc.workspace_id
    );
}

/// The sharing switch is this computer's own choice, and a reboot must not
/// quietly undo it.
#[test]
fn the_sharing_switch_survives_a_restart() {
    let dir = tempfile::tempdir().expect("temp dir");

    // A machine nobody has configured is not sharing: taking over somebody's
    // keyboard is never the default.
    let store = Store::at(dir.path()).expect("store opens");
    assert!(!store.load_prefs().sharing);

    store
        .save_prefs(&LocalPrefs { sharing: true })
        .expect("saved");

    // The next launch: a new Store over the same directory, which is exactly
    // what starting the app again does.
    let restarted = Store::at(dir.path()).expect("store opens");
    assert!(
        restarted.load_prefs().sharing,
        "sharing was on when the machine was switched off, so it must come back on"
    );

    // And it is a local file, not part of the synchronized document — turning
    // it on here must not turn it on for every paired machine.
    assert!(!fs::read_to_string(dir.path().join("workspace.json"))
        .unwrap_or_default()
        .contains("\"sharing\""));
}

/// Preferences are a convenience. A damaged file must not stop the app.
#[test]
fn unreadable_preferences_fall_back_to_the_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::at(dir.path()).expect("store opens");
    store
        .save_prefs(&LocalPrefs { sharing: true })
        .expect("saved");

    fs::write(dir.path().join("local-prefs.json"), b"{ truncated").expect("clobbered");
    fs::write(dir.path().join("local-prefs.json.bak"), b"nonsense").expect("clobbered");

    assert!(!store.load_prefs().sharing, "damaged file, safe default");
}
