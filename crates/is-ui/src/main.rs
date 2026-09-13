#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! InputShare desktop app.
//!
//! The window is a view onto [`is_agent::Agent`], which owns the workspace, the
//! peer connections and what is reachable. Every command here is a thin
//! translation: take an intent from the interface, hand it to the agent, hand
//! back the whole state.
//!
//! Requirement 4 says the input agent must not depend on the UI being open.
//! Today the window hosts the agent in-process; because the agent is a library
//! with no knowledge of the window, moving it into a headless daemon is a change
//! to `main`, not to the interface.

use std::sync::Arc;

use is_agent::{autostart, Agent};
use is_core::layout::{fallback_owner, resolve_cursor};
use is_core::{
    InputAssignment, MachineId, MachineRecord, OfflineEdgeBehavior, Platform, Point, Resolution,
    Store, WorkspaceSettings,
};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

type Shared<'a> = State<'a, Arc<Agent>>;

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

// ---------------------------------------------------------------- view models

#[derive(Serialize)]
struct UiState {
    machine_id: String,
    fingerprint: String,
    config_dir: String,
    transport_port: Option<u16>,
    autostart: bool,
    sharing: bool,
    /// Suggested name for this machine, from the computer's own name.
    computer_name: String,
    discovery_port: u16,
    interfaces: Vec<UiInterface>,
    manual_peers: Vec<String>,
    candidates: Vec<UiCandidate>,
    /// Machines this computer asked to join, still waiting to be admitted from
    /// the other end.
    awaiting_join: Vec<String>,
    workspace: Option<UiWorkspace>,
}

#[derive(Serialize)]
struct UiCandidate {
    machine_id: String,
    display_name: String,
    fingerprint: String,
    addr: String,
    seconds_since_seen: u64,
    has_workspace: bool,
    /// It already admitted this computer; one click finishes the pairing.
    invited_us: bool,
}

/// One network card, as the scan view shows it.
#[derive(Serialize)]
struct UiInterface {
    name: String,
    address: String,
    /// A `169.254.x.x` address: the adapter never got one from a router, so
    /// nothing is likely to be found on it.
    self_assigned: bool,
}

#[derive(Serialize)]
struct UiWorkspace {
    id: String,
    name: String,
    revision: u64,
    settings: UiSettings,
    machines: Vec<UiMachine>,
    cursor_owner: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct UiSettings {
    clipboard_sync: bool,
    offline_edge: String,
    discovery_port: u16,
    transport_port: u16,
    autostart: bool,
}

/// One physical screen, in workspace coordinates.
#[derive(Serialize)]
struct UiScreen {
    name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    primary: bool,
}

#[derive(Serialize)]
struct UiMachine {
    id: String,
    name: String,
    platform: String,
    fingerprint: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    keyboard: bool,
    mouse: bool,
    online: bool,
    is_local: bool,
    /// A machine that has connected at least once has told us its displays. One
    /// that has not is in the workspace but has no geometry yet.
    known_displays: bool,
    /// Every screen it has, so a machine with two monitors looks like one.
    screens: Vec<UiScreen>,
}

fn platform_label(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "Windows",
        Platform::MacOs => "macOS",
        Platform::Linux => "Linux",
        Platform::Other => "Unknown",
    }
}

fn view(agent: &Arc<Agent>) -> UiState {
    let snapshot = agent.view();
    let me = snapshot.identity.machine_id;

    let workspace = snapshot.doc.as_ref().map(|doc| {
        let mut machines: Vec<UiMachine> = doc
            .machines_present()
            .map(|m| {
                let bounds = m.bounds();
                UiMachine {
                    id: m.id.to_string(),
                    name: m.display_name.clone(),
                    platform: platform_label(m.platform).to_string(),
                    fingerprint: m.public_key.fingerprint(),
                    // The corner of everything this machine covers, which is
                    // where it is drawn. With one screen it equals the machine's
                    // position; with a second screen to the left or above, it
                    // does not, and drawing at the position would put the card
                    // in the wrong place.
                    x: bounds.map(|b| b.x).unwrap_or(m.position.x),
                    y: bounds.map(|b| b.y).unwrap_or(m.position.y),
                    width: bounds.map(|b| b.width).unwrap_or(0),
                    height: bounds.map(|b| b.height).unwrap_or(0),
                    keyboard: m.input.provides_keyboard,
                    mouse: m.input.provides_mouse,
                    online: snapshot.online.contains(&m.id) || m.id == me,
                    is_local: m.id == me,
                    known_displays: !m.displays.is_empty(),
                    screens: m
                        .displays
                        .iter()
                        .map(|d| UiScreen {
                            name: d.name.clone(),
                            x: d.x + m.position.x,
                            y: d.y + m.position.y,
                            width: d.width,
                            height: d.height,
                            primary: d.primary,
                        })
                        .collect(),
                }
            })
            .collect();
        machines.sort_by_key(|m| (m.y, m.x));

        let mut online = snapshot.online.clone();
        online.insert(me);
        let settings = &doc.settings.value;

        UiWorkspace {
            id: doc.workspace_id.to_string(),
            name: doc.name.value.clone(),
            revision: doc.lamport,
            settings: UiSettings {
                clipboard_sync: settings.clipboard_sync,
                offline_edge: match settings.offline_edge {
                    OfflineEdgeBehavior::Block => "block".into(),
                    OfflineEdgeBehavior::SkipOver => "skip_over".into(),
                },
                discovery_port: settings.discovery_port,
                transport_port: settings.transport_port,
                autostart: settings.autostart,
            },
            machines,
            // The live one while input is being shared; otherwise the machine
            // that would take the pointer if it had to be placed somewhere.
            cursor_owner: snapshot
                .cursor_owner
                .or_else(|| fallback_owner(doc, &online, me))
                .and_then(|id| doc.machine(id).map(|m| m.display_name.clone())),
        }
    });

    UiState {
        machine_id: me.to_string(),
        fingerprint: snapshot.identity.public_key().fingerprint(),
        config_dir: snapshot.config_dir.display().to_string(),
        transport_port: snapshot.transport_port,
        autostart: autostart::is_enabled(),
        sharing: agent.is_sharing(),
        computer_name: is_agent::computer_name(),
        discovery_port: agent.discovery_port(),
        interfaces: agent
            .interfaces()
            .into_iter()
            .map(|i| UiInterface {
                name: i.name,
                address: i.address.to_string(),
                self_assigned: i.self_assigned,
            })
            .collect(),
        manual_peers: snapshot
            .manual_peers
            .iter()
            .map(|addr| addr.to_string())
            .collect(),
        candidates: snapshot
            .candidates
            .iter()
            .map(|c| UiCandidate {
                machine_id: c.machine_id.to_string(),
                display_name: c.display_name.clone(),
                fingerprint: c.identity_key.fingerprint(),
                addr: c.addr.clone(),
                seconds_since_seen: is_core::now_ms().saturating_sub(c.last_seen_ms) / 1000,
                has_workspace: c.has_workspace,
                invited_us: c.invited_us,
            })
            .collect(),
        awaiting_join: snapshot
            .awaiting_join
            .iter()
            .map(|id| id.to_string())
            .collect(),
        workspace,
    }
}

fn parse_id(id: &str) -> Result<MachineId, String> {
    id.parse::<MachineId>()
        .map_err(|_| format!("not a machine id: {id}"))
}

/// Replaces a field on a machine record and writes the whole record back.
///
/// Coarse on purpose: the document's merge is per machine, so a partial update
/// would still have to become a whole record before it could be shared.
async fn edit_machine(
    agent: &Arc<Agent>,
    id: &str,
    edit: impl FnOnce(&mut MachineRecord),
) -> Result<(), String> {
    let id = parse_id(id)?;
    let mut record = agent
        .view()
        .doc
        .and_then(|doc| doc.machine(id).cloned())
        .ok_or_else(|| "no such machine".to_string())?;
    edit(&mut record);
    agent.upsert_machine(record).await.map_err(message)
}

// -------------------------------------------------------------------- commands

#[tauri::command]
fn get_state(agent: Shared<'_>) -> UiState {
    view(&agent)
}

/// Admits a discovered machine. The other computer must do the same for this
/// one before they will talk: neither side trusts a key it was not told to.
#[tauri::command]
async fn pair(agent: Shared<'_>, id: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    agent.pair(parse_id(&id)?).await.map_err(message)?;
    Ok(view(&agent))
}

#[tauri::command]
async fn unpair(agent: Shared<'_>, id: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    agent.unpair(parse_id(&id)?).await.map_err(message)?;
    Ok(view(&agent))
}

/// Moves a machine so that the corner of everything it covers lands on `x, y`.
///
/// The canvas works in that corner, not in the machine's own origin: with two
/// screens the two are different, and asking the interface to know the
/// difference is how a card ends up somewhere other than where it was dropped.
#[tauri::command]
async fn move_machine(agent: Shared<'_>, id: String, x: i32, y: i32) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    edit_machine(&agent, &id, |record| {
        let offset = record
            .bounds()
            .map(|b| Point::new(b.x - record.position.x, b.y - record.position.y))
            .unwrap_or_default();
        record.position = Point::new(x - offset.x, y - offset.y);
    })
    .await?;
    Ok(view(&agent))
}

#[tauri::command]
async fn rename_machine(agent: Shared<'_>, id: String, name: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    edit_machine(&agent, &id, |record| record.display_name = name).await?;
    Ok(view(&agent))
}

#[tauri::command]
async fn set_input(
    agent: Shared<'_>,
    id: String,
    keyboard: bool,
    mouse: bool,
) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    edit_machine(&agent, &id, |record| {
        record.input = InputAssignment {
            provides_keyboard: keyboard,
            provides_mouse: mouse,
        }
    })
    .await?;
    Ok(view(&agent))
}

#[tauri::command]
async fn rename_workspace(agent: Shared<'_>, name: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    agent.rename_workspace(&name).await.map_err(message)?;
    Ok(view(&agent))
}

#[tauri::command]
async fn update_settings(agent: Shared<'_>, settings: UiSettings) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    let offline_edge = match settings.offline_edge.as_str() {
        "skip_over" => OfflineEdgeBehavior::SkipOver,
        "block" => OfflineEdgeBehavior::Block,
        other => return Err(format!("unknown offline edge behaviour: {other}")),
    };
    let preferred_interfaces = agent
        .view()
        .doc
        .map(|doc| doc.settings.value.preferred_interfaces.clone())
        .unwrap_or_default();

    agent
        .update_settings(WorkspaceSettings {
            clipboard_sync: settings.clipboard_sync,
            offline_edge,
            discovery_port: settings.discovery_port,
            transport_port: settings.transport_port,
            preferred_interfaces,
            autostart: settings.autostart,
        })
        .await
        .map_err(message)?;

    // The setting records the intent; this makes the machine actually do it.
    let executable = autostart::current_executable().map_err(message)?;
    autostart::set(settings.autostart, &executable).map_err(message)?;

    Ok(view(&agent))
}

#[derive(Serialize)]
struct RouteResult {
    outcome: String,
    machine: Option<String>,
    x: i32,
    y: i32,
    explanation: String,
}

#[tauri::command]
fn resolve_route(agent: Shared<'_>, from: String, x: i32, y: i32) -> Result<RouteResult, String> {
    let snapshot = agent.view();
    let doc = snapshot.doc.as_ref().ok_or("no workspace")?;
    let from = parse_id(&from)?;
    let mut online = snapshot.online.clone();
    online.insert(snapshot.identity.machine_id);
    let target = Point::new(x, y);

    Ok(match resolve_cursor(doc, &online, from, target) {
        Resolution::Stay(point) => {
            let left_the_machine = !doc
                .machine(from)
                .and_then(|m| m.bounds())
                .map(|b| b.contains(target))
                .unwrap_or(false);
            RouteResult {
                outcome: if left_the_machine { "blocked" } else { "same" }.into(),
                machine: doc.machine(from).map(|m| m.display_name.clone()),
                x: point.x,
                y: point.y,
                explanation: if left_the_machine {
                    "held at the boundary — nothing online over there".into()
                } else {
                    "still on the same machine".into()
                },
            }
        }
        Resolution::Move { machine, point } => RouteResult {
            outcome: "handoff".into(),
            machine: doc.machine(machine).map(|m| m.display_name.clone()),
            x: point.x,
            y: point.y,
            explanation: "crosses over".into(),
        },
    })
}

/// Renames this machine, as everyone in the workspace sees it.
#[tauri::command]
async fn rename_this_machine(agent: Shared<'_>, name: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("a computer needs a name".into());
    }
    agent.rename_this_machine(&name).await.map_err(message)?;
    Ok(view(&agent))
}

/// Adds an address to contact directly.
///
/// Accepts `192.168.1.20` or `192.168.1.20:47451`. The bare form gets the
/// workspace's discovery port, because that is the number nobody should have to
/// know.
#[tauri::command]
async fn add_manual_peer(agent: Shared<'_>, address: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err("enter an address".into());
    }
    let parsed: std::net::SocketAddr = match trimmed.parse() {
        Ok(addr) => addr,
        Err(_) => format!("{trimmed}:{}", agent.discovery_port())
            .parse()
            .map_err(|_| format!("\"{trimmed}\" is not an address like 192.168.1.20"))?,
    };
    agent.add_manual_peer(parsed).await;
    Ok(view(&agent))
}

#[tauri::command]
async fn forget_manual_peer(agent: Shared<'_>, address: String) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    if let Ok(parsed) = address.parse::<std::net::SocketAddr>() {
        agent.forget_manual_peer(parsed).await;
    }
    Ok(view(&agent))
}

/// Turns keyboard and mouse sharing on or off.
///
/// Off by default and never turned on by anything but this call. While it is on
/// the agent can swallow this machine's keyboard, so it is a deliberate act with
/// a visible switch, not a side effect of pairing.
#[tauri::command]
async fn set_sharing(agent: Shared<'_>, enabled: bool) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    agent.set_sharing(enabled).await.map_err(message)?;
    Ok(view(&agent))
}

/// Announces now and clears every reconnection backoff.
#[tauri::command]
async fn rescan(agent: Shared<'_>) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    agent.rescan().await;
    Ok(view(&agent))
}

/// Re-reads the monitors and publishes them if they changed.
#[tauri::command]
async fn refresh_displays(agent: Shared<'_>) -> Result<UiState, String> {
    let agent = agent.inner().clone();
    agent.publish_local_facts().await.map_err(message)?;
    Ok(view(&agent))
}

#[tauri::command]
fn export_backup(agent: Shared<'_>, path: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(path);
    Store::at(agent.view().config_dir)
        .map_err(message)?
        .export_backup(&path)
        .map_err(message)?;
    Ok(path.display().to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let store = Store::open()?;
            let agent = Agent::load(store)?;
            app.manage(agent.clone());

            // The network changes state without anyone clicking anything: a peer
            // boots, a peer vanishes, a peer sends an edit. The window is told
            // to refresh rather than polling for it.
            let handle = app.handle().clone();
            let mut changes = agent.subscribe();
            tauri::async_runtime::spawn(async move {
                while changes.recv().await.is_ok() {
                    let _ = handle.emit("workspace-changed", ());
                }
            });

            let online = agent.clone();
            let handle_for_sharing = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Publish the real screens before anything else: peers that
                // connect a moment later should see this machine's actual
                // geometry, not the shape it was created with.
                if let Err(error) = online.publish_local_facts().await {
                    tracing::warn!(%error, "could not publish this machine's displays");
                }
                if let Err(error) = online.go_online().await {
                    tracing::warn!(%error, "could not start networking");
                }
                // A machine that was sharing when it was last switched off picks
                // it up again by itself. Requirement 1 is that a reboot changes
                // nothing the user has to redo, and a switch that quietly
                // resets is exactly that.
                if online.sharing_was_on() {
                    match online.set_sharing(true).await {
                        Ok(()) => tracing::info!("sharing resumed from the last session"),
                        // Kept as the stored intent, so the next launch tries
                        // again; on macOS this is usually a permission that has
                        // not been granted yet.
                        Err(error) => tracing::warn!(%error, "could not resume sharing"),
                    }
                    let _ = handle_for_sharing.emit("workspace-changed", ());
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            pair,
            unpair,
            move_machine,
            rename_machine,
            set_input,
            rename_workspace,
            update_settings,
            rescan,
            refresh_displays,
            set_sharing,
            add_manual_peer,
            rename_this_machine,
            forget_manual_peer,
            resolve_route,
            export_backup,
        ])
        .run(tauri::generate_context!())
        .expect("the InputShare window could not start");
}
