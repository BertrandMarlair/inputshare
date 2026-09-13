//! Pairs two config directories, the way two people clicking Pair would.
//!
//!     cargo run -p is-agent --example pair_two -- DIR_A DIR_B
//!
//! Point two copies of the app at those directories afterwards: they find each
//! other and connect on their own. Useful for looking at the interface in a
//! state that would otherwise take two computers and two clicks to reach.

use std::time::Duration;

use is_agent::Agent;
use is_core::Store;

const PORT: u16 = 47451;

async fn wait_for(label: &str, mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..120 {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    eprintln!("gave up waiting: {label}");
    false
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    let [dir_a, dir_b] = dirs.as_slice() else {
        eprintln!("usage: pair_two DIR_A DIR_B");
        std::process::exit(2);
    };

    let a = Agent::load(Store::at(dir_a).expect("store a")).expect("agent a");
    let b = Agent::load(Store::at(dir_b).expect("store b")).expect("agent b");
    a.rename_this_machine("Studio Mac").await.expect("rename a");
    b.rename_this_machine("Studio PC").await.expect("rename b");

    a.go_online_on(PORT, 0).await.expect("a online");
    b.go_online_on(PORT, 0).await.expect("b online");

    let a_id = a.identity().machine_id;
    let b_id = b.identity().machine_id;
    println!("A = {a_id}\nB = {b_id}\n");

    if !wait_for("A to discover B", || {
        a.view().candidates.iter().any(|c| c.machine_id == b_id)
    })
    .await
    {
        std::process::exit(1);
    }
    a.pair(b_id).await.expect("a admits b");
    println!("A admitted B");

    let invited = wait_for("B to be told it was invited", || {
        b.view()
            .candidates
            .iter()
            .any(|c| c.machine_id == a_id && c.invited_us)
    })
    .await;
    println!("B sees the invitation: {invited}");

    b.pair(a_id).await.expect("b accepts");
    println!("B accepted");

    let together = wait_for("them to settle into one configuration", || {
        let av = a.view();
        let bv = b.view();
        av.online.contains(&b_id)
            && bv.online.contains(&a_id)
            && av.doc.map(|d| d.workspace_id) == bv.doc.map(|d| d.workspace_id)
    })
    .await;
    println!("one configuration, both connected: {together}");

    if let Some(doc) = a.view().doc {
        println!("\ncontains:");
        for machine in doc.machines_present() {
            let bounds = machine
                .bounds()
                .map(|b| format!("x {}..{}, y {}..{}", b.x, b.max_x(), b.y, b.max_y()))
                .unwrap_or_else(|| "no screens".into());
            println!("  {:<12} {bounds}", machine.display_name);
        }
    }

    // Let the last writes reach disk before the process ends.
    tokio::time::sleep(Duration::from_millis(500)).await;
}
