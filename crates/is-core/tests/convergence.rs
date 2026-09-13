//! Requirements 9 and 11: reconnecting machines must converge, and stale state
//! must never overwrite newer state.

mod common;

use common::{machine, snapshot};
use is_core::{OfflineEdgeBehavior, Point, WorkspaceDoc, WorkspaceSettings};

#[test]
fn update_order_does_not_matter() {
    let mac = machine("Mac", Point::new(-1920, 0));
    let pc1 = machine("PC1", Point::new(0, 0));
    let pc2 = machine("PC2", Point::new(0, -1080));

    // PC1 founds the workspace. Two replicas branch off before any change, then
    // receive the same three updates in opposite orders.
    let mut author = WorkspaceDoc::create("My Desk", pc1.clone());
    let mut forward = author.clone();
    let mut reverse = author.clone();

    let add_mac = author.upsert_machine(pc1.id, mac.clone());
    let add_pc2 = author.upsert_machine(mac.id, pc2.clone());
    let rename = author.set_name(pc2.id, "Studio");

    for update in [add_mac.clone(), add_pc2.clone(), rename.clone()] {
        forward.apply(update).expect("same workspace");
    }
    for update in [rename, add_pc2, add_mac] {
        reverse.apply(update).expect("same workspace");
    }

    assert_eq!(snapshot(&forward), snapshot(&reverse));
    assert_eq!(forward.name.value, "Studio");
    assert_eq!(forward.machines_present().count(), 3);
}

#[test]
fn merging_twice_changes_nothing() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let mac = machine("Mac", Point::new(-1920, 0));

    let mut a = WorkspaceDoc::create("My Desk", pc1.clone());
    let mut b = a.clone();
    a.upsert_machine(pc1.id, mac);

    assert!(b.merge(&a).expect("same workspace"), "first merge applies");
    assert!(
        !b.merge(&a).expect("same workspace"),
        "second merge is a no-op"
    );
    assert_eq!(snapshot(&a), snapshot(&b));
}

#[test]
fn stale_document_cannot_clobber_newer_state() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let mut current = WorkspaceDoc::create("My Desk", pc1.clone());

    // PC2 has been powered off since before the rename.
    let stale = current.clone();
    current.set_name(pc1.id, "Studio");

    let mut reconnecting = current.clone();
    reconnecting.merge(&stale).expect("same workspace");

    assert_eq!(
        reconnecting.name.value, "Studio",
        "the week-old copy must not win"
    );
}

#[test]
fn offline_machine_stays_in_the_workspace() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let pc2 = machine("PC2", Point::new(0, -1080));

    let mut doc = WorkspaceDoc::create("My Desk", pc1.clone());
    doc.upsert_machine(pc1.id, pc2.clone());

    // PC2 is powered off and never heard from again. Liveness is not part of the
    // document, so repeated syncs between the machines that are up cannot drop
    // it.
    let peer_view = doc.clone();
    doc.merge(&peer_view).expect("same workspace");

    assert!(doc.machine(pc2.id).is_some());
    assert!(!doc.is_removed(pc2.id));
    assert_eq!(doc.machines_present().count(), 2);
}

#[test]
fn unpairing_survives_a_merge_with_a_peer_that_still_remembers() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let pc2 = machine("PC2", Point::new(0, -1080));

    let mut pc1_doc = WorkspaceDoc::create("My Desk", pc1.clone());
    pc1_doc.upsert_machine(pc1.id, pc2.clone());
    let pc2_doc = pc1_doc.clone();

    pc1_doc.remove_machine(pc1.id, pc2.id);

    // PC2 reconnects still carrying itself as present.
    pc1_doc.merge(&pc2_doc).expect("same workspace");

    assert!(
        pc1_doc.is_removed(pc2.id),
        "tombstone must outlive the merge, or unpairing never sticks"
    );
    assert!(pc1_doc.machine(pc2.id).is_none());
}

#[test]
fn concurrent_edits_resolve_the_same_way_on_both_machines() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let mac = machine("Mac", Point::new(-1920, 0));

    let mut base = WorkspaceDoc::create("My Desk", pc1.clone());
    base.upsert_machine(pc1.id, mac.clone());

    // Both machines rename the workspace at the same logical time, each unaware
    // of the other.
    let mut from_pc1 = base.clone();
    let mut from_mac = base.clone();
    let pc1_update = from_pc1.set_name(pc1.id, "Desk A");
    let mac_update = from_mac.set_name(mac.id, "Desk B");

    from_pc1.apply(mac_update).expect("same workspace");
    from_mac.apply(pc1_update).expect("same workspace");

    assert_eq!(
        from_pc1.name.value, from_mac.name.value,
        "machine ID breaks the tie identically on both sides"
    );
    assert_eq!(snapshot(&from_pc1), snapshot(&from_mac));
}

#[test]
fn settings_changes_propagate_and_converge() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let mut a = WorkspaceDoc::create("My Desk", pc1.clone());
    let mut b = a.clone();

    let update = a.set_settings(
        pc1.id,
        WorkspaceSettings {
            offline_edge: OfflineEdgeBehavior::SkipOver,
            clipboard_sync: false,
            ..WorkspaceSettings::default()
        },
    );
    b.apply(update).expect("same workspace");

    assert_eq!(b.settings.value.offline_edge, OfflineEdgeBehavior::SkipOver);
    assert!(!b.settings.value.clipboard_sync);
    assert_eq!(snapshot(&a), snapshot(&b));
}

#[test]
fn updates_from_another_workspace_are_rejected() {
    let pc1 = machine("PC1", Point::new(0, 0));
    let mut mine = WorkspaceDoc::create("My Desk", pc1.clone());
    let mut theirs = WorkspaceDoc::create("Their Desk", machine("Other", Point::default()));

    let foreign = theirs.set_name(theirs.machines.keys().next().copied().unwrap(), "Nope");

    assert!(mine.apply(foreign).is_err());
    assert!(mine.merge(&theirs).is_err());
    assert_eq!(mine.name.value, "My Desk");
}
