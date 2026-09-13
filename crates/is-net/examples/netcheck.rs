//! Says out loud what discovery is doing, and on which network cards.
//!
//!     cargo run -p is-net --example netcheck
//!     cargo run -p is-net --example netcheck -- 192.168.86.42
//!
//! Run it on both machines at once. Each should list its interfaces, then print
//! the other's announcements as they arrive. If a machine only ever sees itself,
//! the two are not on the same network segment — or something between them is
//! dropping multicast, which is what the optional address argument is for: it
//! adds that machine as a direct target and asks it to answer.
//!
//! An address argument also gets a direct TCP test against the transport port,
//! because discovery and pairing fail in different ways for different reasons:
//! discovery is UDP and can be eaten by an access point that drops multicast,
//! while pairing is an ordinary TCP connection and is usually a firewall.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use is_core::Identity;
use is_net::discovery::{self, Sighting};
use is_net::wire::Announcement;
use tokio::sync::{mpsc, Mutex, Notify};

const PORT: u16 = 47451;
/// The port a peer accepts pairing and sync connections on.
const TRANSPORT_PORT: u16 = 47452;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let identity = Identity::generate();
    println!("this probe is machine {}", identity.machine_id);

    let interfaces = discovery::interfaces();
    if interfaces.is_empty() {
        println!("\nNo usable IPv4 interface. Nothing can be discovered.");
        return;
    }

    println!("\nInterfaces discovery will use:");
    for interface in &interfaces {
        let note = if interface.self_assigned {
            "  (self-assigned — this adapter never got an address from a router)"
        } else {
            ""
        };
        let broadcast = interface
            .broadcast
            .map(|b| format!(", broadcast {b}"))
            .unwrap_or_default();
        println!(
            "  {:<38} {}{}{}",
            interface.name, interface.address, broadcast, note
        );
    }
    println!(
        "\nAnnouncing on all {} of them, every 2 seconds.",
        interfaces.len()
    );

    let manual: discovery::ManualPeers = Arc::new(Mutex::new(Vec::new()));
    for argument in std::env::args().skip(1) {
        let target: SocketAddr = match argument.parse() {
            Ok(addr) => addr,
            Err(_) => match format!("{argument}:{PORT}").parse() {
                Ok(addr) => addr,
                Err(_) => {
                    eprintln!("not an address: {argument}");
                    continue;
                }
            },
        };
        println!("Also asking {target} directly.");
        manual.lock().await.push(target);
        probe_transport(target.ip()).await;
    }

    let machine_id = identity.machine_id;
    let key = identity.public_key();
    tokio::spawn(discovery::announce(
        PORT,
        Duration::from_secs(2),
        Arc::new(Notify::new()),
        manual,
        move || Announcement {
            protocol: 1,
            machine_id,
            identity_key: key,
            display_name: "netcheck probe".into(),
            workspace_id: None,
            transport_port: 0,
            probe: false,
            invitations: Vec::new(),
        },
    ));

    let (tx, mut rx) = mpsc::channel::<Sighting>(64);
    tokio::spawn(discovery::listen(PORT, tx));

    println!("\nListening. Announcements will appear below.\n");
    let mut seen_self = false;
    let mut seen_others = 0;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(sighting)) => {
                let announcement = sighting.announcement;
                if announcement.machine_id == machine_id {
                    if !seen_self {
                        seen_self = true;
                        println!(
                            "  [self]  our own announcement came back from {}",
                            sighting.from
                        );
                    }
                    continue;
                }
                seen_others += 1;
                let invites = if announcement.invitations.is_empty() {
                    "no invitations".to_string()
                } else {
                    format!(
                        "inviting {}",
                        announcement
                            .invitations
                            .iter()
                            .map(|id| id.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                println!(
                    "  [peer]  {} at {} — machine {} — {invites}",
                    announcement.display_name, sighting.from, announcement.machine_id
                );
            }
            Ok(None) | Err(_) => break,
        }
    }

    println!("\n--- after 30 seconds ---");
    if !seen_self {
        println!("We never heard our own announcement. Multicast is not working on");
        println!("this machine at all — check whether a firewall is blocking UDP {PORT}.");
    }
    if seen_others == 0 {
        println!("No other machine was heard.");
        println!("  - Is the other machine running this too, at the same time?");
        println!("  - Are both on the same network? Compare the addresses listed above.");
        println!("  - Guest or public Wi-Fi often isolates clients from each other.");
        println!("  - Pass the other machine's address as an argument to reach it directly.");
        #[cfg(target_os = "macos")]
        {
            println!("  - On macOS 15 and later, every application needs permission to");
            println!("    talk to the local network, and is refused in silence until it");
            println!("    has it. Check System Settings > Privacy & Security > Local");
            println!("    Network. This probe and the app are asked for separately.");
        }
    } else {
        println!("Heard {seen_others} announcement(s) from other machines. Discovery works.");
    }
}

/// Can we open the connection pairing actually uses?
///
/// Discovery working and pairing working are different questions with different
/// answers: one is UDP to a group, the other is a TCP connection to a host. A
/// machine that appears in the list and then refuses to pair has usually been
/// found by the first and blocked on the second.
async fn probe_transport(ip: std::net::IpAddr) {
    let target = SocketAddr::new(ip, TRANSPORT_PORT);
    print!("  connecting to {target} (the port pairing uses)… ");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(target),
    )
    .await
    {
        Ok(Ok(_)) => println!("open. Pairing can reach this machine."),
        Ok(Err(error)) => {
            println!("refused: {error}");
            println!("    The machine answered, so the network is fine and nothing is");
            println!("    listening: InputShare is probably not running over there.");
        }
        Err(_) => {
            println!("no answer after 5s.");
            println!("    Something is dropping the connection rather than refusing it,");
            println!("    which is what a firewall looks like. On macOS the first");
            println!("    incoming connection needs approval, and an app that was denied");
            println!("    once is denied silently afterwards.");
        }
    }
}
