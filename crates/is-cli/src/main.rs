//! Command line for the InputShare workspace core.
//!
//! Until `is-net` exists there is nothing to connect to, so `status` reports
//! every peer as offline and `demo` simulates the peers instead of discovering
//! them. What is real here is the workspace itself: it is created, persisted,
//! reloaded from disk as a reboot would, merged, and routed against.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use is_core::layout::{fallback_owner, resolve_cursor};
use is_core::{
    DisplayInfo, Identity, InputAssignment, MachineId, MachineRecord, OfflineEdgeBehavior,
    Platform, Point, Resolution, Store, WorkspaceDoc, WorkspaceSettings,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("demo");

    let result = match command {
        "demo" => demo(),
        "status" => status(),
        "init" => match args.get(1) {
            Some(name) => init(name),
            None => usage("init needs a workspace name"),
        },
        "add" => match (args.get(1), args.get(2), args.get(3)) {
            (Some(name), Some(x), Some(y)) => match (x.parse(), y.parse()) {
                (Ok(x), Ok(y)) => add(name, Point::new(x, y)),
                _ => usage("add needs integer x and y"),
            },
            _ => usage("add needs a name, an x and a y"),
        },
        "route" => match (args.get(1), args.get(2)) {
            (Some(x), Some(y)) => match (x.parse(), y.parse()) {
                (Ok(x), Ok(y)) => route(Point::new(x, y)),
                _ => usage("route needs integer x and y"),
            },
            _ => usage("route needs an x and a y"),
        },
        "help" | "--help" | "-h" => usage(""),
        other => usage(&format!("unknown command: {other}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage(problem: &str) -> is_core::Result<()> {
    if !problem.is_empty() {
        eprintln!("{problem}\n");
    }
    eprintln!(
        "inputshare <command>

  demo                 walk the workspace through create, reboot and an offline peer
  status               show this machine and the stored workspace
  init <name>          create a workspace with this machine as its first member
  add <name> <x> <y>   add a machine at a position, in workspace pixels
  route <x> <y>        resolve a cursor move from this machine to a point

Config lives in the platform config directory, or INPUTSHARE_CONFIG_DIR if set."
    );
    Ok(())
}

/// A stand-in display, until `is-input` can report the real ones.
fn placeholder_display() -> DisplayInfo {
    DisplayInfo {
        id: "display-0".into(),
        name: "Primary (placeholder)".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.0,
        primary: true,
    }
}

fn record(
    id: MachineId,
    name: &str,
    position: Point,
    key: is_core::PubKey,
    platform: Platform,
) -> MachineRecord {
    MachineRecord {
        id,
        display_name: name.into(),
        platform,
        public_key: key,
        position,
        displays: vec![placeholder_display()],
        input: InputAssignment {
            provides_keyboard: true,
            provides_mouse: true,
        },
        paired_at_ms: is_core::now_ms(),
    }
}

fn local_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "This machine".into())
}

fn init(name: &str) -> is_core::Result<()> {
    let store = Store::open()?;
    let identity = store.load_or_create_identity()?;

    if let Some(existing) = store.load_workspace()? {
        println!(
            "already in workspace \"{}\" ({}). Nothing to do.",
            existing.name.value, existing.workspace_id
        );
        return Ok(());
    }

    let me = record(
        identity.machine_id,
        &local_name(),
        Point::new(0, 0),
        identity.public_key(),
        Platform::current(),
    );
    let doc = WorkspaceDoc::create(name, me);
    store.save_workspace(&doc)?;

    println!(
        "created workspace \"{}\" ({})",
        doc.name.value, doc.workspace_id
    );
    println!("stored in {}", store.dir().display());
    Ok(())
}

fn add(name: &str, position: Point) -> is_core::Result<()> {
    let store = Store::open()?;
    let identity = store.load_or_create_identity()?;
    let Some(mut doc) = store.load_workspace()? else {
        println!("no workspace yet — run `inputshare init <name>` first");
        return Ok(());
    };

    // A real pairing exchanges keys with the other machine. This mints a
    // placeholder identity so the topology can be laid out before `is-net`
    // exists.
    let peer = Identity::generate();
    // Unknown until a real pairing tells us: this process is only inventing a
    // placeholder for the other computer.
    let peer_record = record(
        peer.machine_id,
        name,
        position,
        peer.public_key(),
        Platform::Other,
    );
    doc.upsert_machine(identity.machine_id, peer_record);
    store.save_workspace(&doc)?;

    println!("added \"{name}\" at ({}, {})", position.x, position.y);
    Ok(())
}

fn status() -> is_core::Result<()> {
    let store = Store::open()?;
    let identity = store.load_or_create_identity()?;

    println!("machine   {}", identity.machine_id);
    println!("key       {}", identity.public_key().fingerprint());
    println!("config    {}", store.dir().display());

    match store.load_workspace()? {
        None => println!("\nnot in a workspace yet — run `inputshare init <name>`"),
        Some(doc) => {
            println!(
                "\nworkspace \"{}\" ({}), revision {}",
                doc.name.value, doc.workspace_id, doc.lamport
            );
            // Only the local machine can be known online without `is-net`.
            let online = HashSet::from([identity.machine_id]);
            print_topology(&doc, &online, identity.machine_id);
            println!("\nPeers show offline because discovery is not implemented yet.");
        }
    }
    Ok(())
}

fn route(target: Point) -> is_core::Result<()> {
    let store = Store::open()?;
    let identity = store.load_or_create_identity()?;
    let Some(doc) = store.load_workspace()? else {
        println!("no workspace yet — run `inputshare init <name>` first");
        return Ok(());
    };

    let online = HashSet::from([identity.machine_id]);
    describe_route(&doc, &online, identity.machine_id, target);
    Ok(())
}

/// Requirement 15: the topology stays visible whatever is up.
fn print_topology(doc: &WorkspaceDoc, online: &HashSet<MachineId>, local: MachineId) {
    let mut machines: Vec<_> = doc.machines_present().collect();
    // Reading order rather than UUID order, so the list resembles the desk.
    machines.sort_by_key(|m| (m.position.y, m.position.x));
    for machine in machines {
        let (glyph, state) = if online.contains(&machine.id) {
            ('\u{25cf}', "online")
        } else {
            ('\u{25cb}', "offline")
        };
        let here = if machine.id == local {
            "  (this machine)"
        } else {
            ""
        };
        let bounds = machine
            .bounds()
            .map(|b| format!("x {}..{}, y {}..{}", b.x, b.max_x(), b.y, b.max_y()))
            .unwrap_or_else(|| "no displays reported".into());
        println!(
            "   {glyph} {:<14} {:<8} {bounds}{here}",
            machine.display_name, state
        );
    }
}

fn describe_route(doc: &WorkspaceDoc, online: &HashSet<MachineId>, from: MachineId, target: Point) {
    let name_of = |id: MachineId| {
        doc.machine(id)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| id.to_string())
    };

    match resolve_cursor(doc, online, from, target) {
        Resolution::Stay(point) => println!(
            "   cursor to ({}, {}) -> stays on {} at ({}, {})",
            target.x,
            target.y,
            name_of(from),
            point.x,
            point.y
        ),
        Resolution::Move { machine, point } => println!(
            "   cursor to ({}, {}) -> hands off to {} at ({}, {})",
            target.x,
            target.y,
            name_of(machine),
            point.x,
            point.y
        ),
    }
}

/// Walks the requirements that can be demonstrated without a network: a
/// workspace is created and persisted, survives a simulated reboot, keeps
/// offline machines in its topology, refuses to strand the cursor on one, and
/// converges after a stale peer reconnects.
fn demo() -> is_core::Result<()> {
    let dir = demo_dir();
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|source| is_core::Error::Io {
            path: dir.clone(),
            source,
        })?;
    }
    let store = Store::at(&dir)?;

    println!("InputShare demo");
    println!("config {}\n", store.dir().display());

    // 1. First launch: identity is minted once and then fixed.
    let identity = store.load_or_create_identity()?;
    let again = store.load_or_create_identity()?;
    println!("1. machine identity");
    println!("   id          {}", identity.machine_id);
    println!("   key         {}", identity.public_key().fingerprint());
    println!(
        "   stable      {}\n",
        identity.machine_id == again.machine_id
    );

    // 2. The layout from the requirements: Mac left of PC1, PC2 above PC1.
    let pc1 = record(
        identity.machine_id,
        "PC1",
        Point::new(0, 0),
        identity.public_key(),
        Platform::current(),
    );
    let mac_identity = Identity::generate();
    let pc2_identity = Identity::generate();
    let mac = record(
        mac_identity.machine_id,
        "Mac",
        Point::new(-1920, 0),
        mac_identity.public_key(),
        Platform::MacOs,
    );
    let pc2 = record(
        pc2_identity.machine_id,
        "PC2",
        Point::new(0, -1080),
        pc2_identity.public_key(),
        Platform::Windows,
    );

    let mut doc = WorkspaceDoc::create("My Desk", pc1.clone());
    doc.upsert_machine(pc1.id, mac.clone());
    doc.upsert_machine(pc1.id, pc2.clone());
    store.save_workspace(&doc)?;

    let everyone = HashSet::from([pc1.id, mac.id, pc2.id]);
    println!(
        "2. workspace \"{}\" at revision {}",
        doc.name.value, doc.lamport
    );
    print_topology(&doc, &everyone, pc1.id);

    // 3. Reboot: nothing in memory, everything from disk.
    let rebooted = Store::at(&dir)?;
    let loaded = rebooted
        .load_workspace()?
        .expect("the workspace was persisted");
    println!("\n3. after a reboot (reloaded from disk)");
    println!(
        "   workspace   \"{}\" ({})",
        loaded.name.value, loaded.workspace_id
    );
    println!("   revision    {}", loaded.lamport);
    println!("   machines    {}", loaded.machines_present().count());

    // 4. PC2 is off. It keeps its place, and the cursor is not sent to it.
    let mut doc = loaded;
    let without_pc2 = HashSet::from([pc1.id, mac.id]);
    println!("\n4. PC2 powered off");
    print_topology(&doc, &without_pc2, pc1.id);
    println!("   routing from PC1:");
    describe_route(&doc, &without_pc2, pc1.id, Point::new(800, -10));
    describe_route(&doc, &without_pc2, pc1.id, Point::new(-5, 400));

    // 5. PC2 returns. The configured route comes back untouched.
    println!("\n5. PC2 back online");
    print_topology(&doc, &everyone, pc1.id);
    println!("   routing from PC1:");
    describe_route(&doc, &everyone, pc1.id, Point::new(800, -10));

    // 6. SkipOver, the other half of requirement 8.
    doc.set_settings(
        pc1.id,
        WorkspaceSettings {
            offline_edge: OfflineEdgeBehavior::SkipOver,
            ..WorkspaceSettings::default()
        },
    );
    println!("\n6. same edge with offline_edge = skip_over");
    describe_route(&doc, &without_pc2, pc1.id, Point::new(800, -10));
    println!("   (nothing is configured beyond PC2, so it still stops)");
    doc.set_settings(pc1.id, WorkspaceSettings::default());

    // 7. The user moves the Mac while PC2 is off. PC2 later reconnects with a
    //    stale copy and must not undo the change.
    let stale_pc2_view = doc.clone();
    let mut moved = mac.clone();
    moved.position = Point::new(0, 1080); // below PC1 instead of left of it
    doc.upsert_machine(pc1.id, moved);
    store.save_workspace(&doc)?;
    println!(
        "\n7. Mac moved from left-of-PC1 to below-PC1, revision {}",
        doc.lamport
    );

    let changed = doc.merge(&stale_pc2_view)?;
    let mac_now = doc.machine(mac.id).expect("the Mac is still a member");
    println!("   PC2 reconnects with revision {}", stale_pc2_view.lamport);
    println!("   stale copy changed anything: {changed}");
    println!(
        "   Mac position stays ({}, {})",
        mac_now.position.x, mac_now.position.y
    );

    // 8. Cursor ownership never sits on a machine that is down.
    println!("\n8. cursor ownership");
    let owner = fallback_owner(&doc, &without_pc2, pc2.id);
    println!(
        "   PC2 held the cursor and went offline -> {}",
        owner
            .and_then(|id| doc.machine(id).map(|m| m.display_name.clone()))
            .unwrap_or_else(|| "nobody online".into())
    );

    println!("\nDone. Discovery, transport and input capture are not built yet.");
    Ok(())
}

fn demo_dir() -> PathBuf {
    std::env::temp_dir().join("inputshare-demo")
}
