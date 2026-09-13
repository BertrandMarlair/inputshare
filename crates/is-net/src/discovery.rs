//! Continuous peer discovery over the local network (requirement 5).
//!
//! Two halves, both running for the life of the process:
//!
//! - **Announce** — say who we are, every few seconds, forever.
//! - **Listen** — accept announcements at any time.
//!
//! It is deliberately not a scan. A scan answers "who is up right now", which is
//! the wrong question for a workspace whose machines boot at different times,
//! associate to Wi-Fi slowly, or have an Ethernet cable plugged in an hour
//! later. Announcing on a timer means a machine that appears at any moment is
//! found within one interval, with no retry logic anywhere else in the system.
//!
//! ## Every interface, explicitly
//!
//! The one thing this module must not do is let the routing table choose the
//! network card. Joining or sending with `INADDR_ANY` delegates that to
//! interface metrics, and on an ordinary machine the winner is routinely a
//! VirtualBox host-only adapter, a WSL bridge or a Docker network — all real
//! interfaces, all with better metrics than Wi-Fi, none with another computer on
//! them. The symptom is total silence between two machines sitting on the same
//! Wi-Fi, and nothing in any log to explain it.
//!
//! So: join the group on **every** IPv4 interface, and send one copy out of
//! **each**, with the outgoing interface set explicitly. A few wasted datagrams
//! on a virtual adapter cost nothing. Picking the wrong one costs the product.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, info, warn};

use crate::wire::Announcement;
use crate::Result;

/// An administratively scoped multicast group: routable inside a site, never
/// forwarded onto the internet.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 98);

/// How long a machine can be silent before we stop believing the address we last
/// saw it at. Several announce intervals, so one dropped datagram is not treated
/// as a machine going away.
pub const HINT_TTL: Duration = Duration::from_secs(20);

/// Addresses a person typed in by hand, for networks where multicast does not
/// survive: guest Wi-Fi with client isolation, some corporate switches, and any
/// two machines on different subnets.
pub type ManualPeers = Arc<Mutex<Vec<SocketAddr>>>;

/// One usable IPv4 interface on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub address: Ipv4Addr,
    /// Broadcast address of this interface's subnet, when it has one.
    pub broadcast: Option<Ipv4Addr>,
    /// A `169.254.x.x` address: an adapter that never got a lease. Kept —
    /// two machines on a dumb switch really do talk over these — but reported,
    /// so the interface can explain a quiet network instead of just shrugging.
    pub self_assigned: bool,
}

/// Every IPv4 interface worth announcing on.
pub fn interfaces() -> Vec<Interface> {
    let Ok(found) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut interfaces: Vec<Interface> = found
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .filter_map(|interface| {
            let if_addrs::IfAddr::V4(v4) = interface.addr else {
                return None;
            };
            let octets = v4.ip.octets();
            Some(Interface {
                name: interface.name,
                address: v4.ip,
                broadcast: v4.broadcast,
                self_assigned: octets[0] == 169 && octets[1] == 254,
            })
        })
        .collect();
    interfaces.sort_by_key(|interface| interface.address);
    interfaces.dedup_by(|a, b| a.address == b.address);
    interfaces
}

/// The listening socket: bound to the well-known port, subscribed to the group
/// on every interface.
///
/// `SO_REUSEADDR` is not a convenience. Every member of a multicast group binds
/// the same port, and it is also what lets a second copy of the app run on one
/// machine during development.
fn bind_listener(port: u16, on: &[Interface]) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    let _ = socket.set_broadcast(true);
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;

    let mut joined = 0;
    for interface in on {
        // Failures here are expected and harmless: a Bluetooth PAN or an
        // unplugged NIC refuses the join. What matters is that the real
        // interface is in the list — and it is, because we try all of them.
        match socket.join_multicast_v4(&GROUP, &interface.address) {
            Ok(()) => joined += 1,
            Err(error) => debug!(
                interface = %interface.name,
                address = %interface.address,
                %error,
                "discovery: could not join the group on this interface"
            ),
        }
    }
    if joined == 0 && !on.is_empty() {
        warn!("discovery: could not join the multicast group on any interface");
    }
    let _ = socket.set_multicast_loop_v4(true);
    Ok(UdpSocket::from_std(socket.into())?)
}

/// A sender bound to one interface, so its datagrams leave by that card and no
/// other.
fn bind_sender(address: Ipv4Addr) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    let _ = socket.set_broadcast(true);
    socket.set_multicast_if_v4(&address)?;
    let _ = socket.set_multicast_loop_v4(true);
    // Enough hops to cross a switch or two, never enough to leave the site.
    let _ = socket.set_multicast_ttl_v4(4);
    socket.bind(&SocketAddr::from((address, 0)).into())?;
    Ok(UdpSocket::from_std(socket.into())?)
}

/// What the listener saw.
#[derive(Clone, Debug)]
pub struct Sighting {
    pub announcement: Announcement,
    pub from: SocketAddr,
}

/// Announces on every interface, every `interval`, forever.
///
/// `describe` is a closure rather than a fixed value because the name and
/// workspace change while running, and a stale announcement would advertise a
/// workspace this machine has already left.
pub async fn announce(
    port: u16,
    interval: Duration,
    wake: Arc<Notify>,
    manual: ManualPeers,
    describe: impl Fn() -> Announcement + Send + 'static,
) {
    let mut senders: HashMap<Ipv4Addr, UdpSocket> = HashMap::new();
    let mut known: Vec<Interface> = Vec::new();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Either the timer, or someone pressing "search the network" — worth
        // having even though discovery is automatic: to a person waiting for a
        // machine to appear, one interval feels like a hang.
        tokio::select! {
            _ = ticker.tick() => {}
            _ = wake.notified() => {}
        }

        let current = interfaces();
        if current != known {
            // A Wi-Fi reconnect, a new lease, a cable, a VPN coming up. Rebuild,
            // rather than keep sending from an address that no longer exists.
            senders.retain(|address, _| current.iter().any(|i| i.address == *address));
            for interface in &current {
                if senders.contains_key(&interface.address) {
                    continue;
                }
                match bind_sender(interface.address) {
                    Ok(socket) => {
                        senders.insert(interface.address, socket);
                    }
                    Err(error) => debug!(
                        interface = %interface.name,
                        %error,
                        "discovery: cannot announce from this interface"
                    ),
                }
            }
            info!(
                count = senders.len(),
                "discovery: announcing on {}",
                current
                    .iter()
                    .map(|i| format!("{} ({})", i.name, i.address))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            known = current;
        }

        let announcement = describe();
        let Ok(payload) = serde_json::to_vec(&announcement) else {
            warn!("discovery: could not encode the announcement");
            continue;
        };

        let group = SocketAddrV4::new(GROUP, port);
        for interface in &known {
            let Some(socket) = senders.get(&interface.address) else {
                continue;
            };
            if let Err(error) = socket.send_to(&payload, group).await {
                debug!(interface = %interface.name, %error, "discovery: multicast send failed");
            }
            // Broadcast as well as multicast. Some access points drop multicast
            // between wireless clients while still passing broadcast, and both
            // together cost one extra small datagram per interface.
            if let Some(broadcast) = interface.broadcast {
                let _ = socket
                    .send_to(&payload, SocketAddrV4::new(broadcast, port))
                    .await;
            }
        }

        // Addresses somebody typed in. These ask for a reply, so entering an
        // address on one machine is enough to introduce both.
        let targets = manual.lock().await.clone();
        if !targets.is_empty() {
            let mut probe = announcement;
            probe.probe = true;
            if let Ok(payload) = serde_json::to_vec(&probe) {
                for target in targets {
                    send_unicast(&senders, &known, target, &payload).await;
                }
            }
        }
    }
}

/// Sends one datagram to a specific address, from whichever interface shares its
/// subnet if any does.
pub(crate) async fn send_unicast(
    senders: &HashMap<Ipv4Addr, UdpSocket>,
    interfaces: &[Interface],
    target: SocketAddr,
    payload: &[u8],
) {
    let IpAddr::V4(target_ip) = target.ip() else {
        return;
    };
    if let Some(address) = interfaces
        .iter()
        .find(|interface| same_subnet(interface, target_ip))
        .map(|interface| interface.address)
    {
        if let Some(socket) = senders.get(&address) {
            let _ = socket.send_to(payload, target).await;
            return;
        }
    }
    // No obvious match: try them all rather than guess. A typed-in address is
    // someone telling us the automatic path already failed.
    for socket in senders.values() {
        let _ = socket.send_to(payload, target).await;
    }
}

/// Crude but adequate: a /24 match is what home and small-office networks look
/// like, and being wrong only means sending from every interface instead of one.
fn same_subnet(interface: &Interface, target: Ipv4Addr) -> bool {
    let a = interface.address.octets();
    let b = target.octets();
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2]
}

/// Replies to a machine that asked to be introduced, once.
pub(crate) async fn reply_to_probe(port: u16, to: IpAddr, announcement: &Announcement) {
    let Ok(payload) = serde_json::to_vec(announcement) else {
        return;
    };
    let interfaces = interfaces();
    let mut senders = HashMap::new();
    for interface in &interfaces {
        if let Ok(socket) = bind_sender(interface.address) {
            senders.insert(interface.address, socket);
        }
    }
    send_unicast(&senders, &interfaces, SocketAddr::new(to, port), &payload).await;
}

/// Listens for announcements, forever, forwarding each to `sink`.
///
/// Rebinds when the set of interfaces changes. An address that appears after
/// startup — Wi-Fi associating late, a cable, a VPN — has to be joined too, or
/// this machine stays deaf on the only network that matters.
pub async fn listen(port: u16, sink: mpsc::Sender<Sighting>) {
    let mut buffer = vec![0u8; 8192];

    loop {
        let on = interfaces();
        let socket = match bind_listener(port, &on) {
            Ok(socket) => socket,
            Err(error) => {
                debug!(%error, "discovery: cannot listen yet, retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        info!(
            port,
            interfaces = on.len(),
            "discovery: listening for announcements"
        );

        let mut recheck = tokio::time::interval(Duration::from_secs(5));
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        recheck.tick().await;

        loop {
            tokio::select! {
                received = socket.recv_from(&mut buffer) => match received {
                    Ok((len, from)) => {
                        // Anyone can send anything here, so this is parsed
                        // defensively and trusted for nothing.
                        let Ok(announcement) =
                            serde_json::from_slice::<Announcement>(&buffer[..len])
                        else {
                            continue;
                        };
                        if !announcement.is_current() {
                            continue;
                        }
                        if sink.send(Sighting { announcement, from }).await.is_err() {
                            return; // the service is gone
                        }
                    }
                    Err(error) => {
                        debug!(%error, "discovery: listener failed, rebinding");
                        break;
                    }
                },
                _ = recheck.tick() => {
                    if interfaces() != on {
                        debug!("discovery: interfaces changed, rejoining");
                        break;
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}
