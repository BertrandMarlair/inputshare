# Persistent Workspace and Automatic Reconnection

This is a core requirement of the application.

The application must behave as a persistent multi-computer workspace rather than requiring manual configuration after every reboot.

## 1. Persistent machine identity

Every installation must have a permanent cryptographically random machine ID generated on first launch.

Example:

```text
machine_id = "7f82c9d1-..."
```

This ID must remain unchanged across:

- application restarts
- computer reboots
- IP address changes
- DHCP changes
- Wi-Fi reconnects
- Ethernet reconnects

The machine ID must NOT depend on:

- IP address
- hostname
- MAC address
- network interface
- current connection

Hostnames and IP addresses are only runtime metadata.

---

## 2. Persistent workspace configuration

The application must persist the complete workspace configuration locally.

Persist at minimum:

- trusted machines
- machine IDs
- machine display configuration
- display positions
- display dimensions
- display scaling information where appropriate
- machine-to-machine topology
- input-device assignments
- keyboard assignments
- mouse assignments
- clipboard synchronization settings
- network preferences
- preferred network interfaces
- application settings
- pairing/trust information

The workspace must survive complete shutdown and reboot of every machine.

---

## 3. Workspace topology must not depend on machine availability

A machine being offline must NOT remove it from the workspace.

Example configured workspace:

```text
                  ┌─────────────┐
                  │     PC2     │
                  └─────────────┘

┌─────────────┐   ┌─────────────┐
│     Mac     │   │     PC1     │
└─────────────┘   └─────────────┘
```

If PC2 is offline:

```text
                  ┌─────────────┐
                  │ PC2 OFFLINE │
                  └─────────────┘

┌─────────────┐   ┌─────────────┐
│     Mac     │   │     PC1     │
└─────────────┘   └─────────────┘
```

The PC2 node must remain in the workspace.

When PC2 comes back online, it must automatically reconnect and become active again without requiring reconfiguration.

---

## 4. Automatic startup

The application must support starting automatically when the operating system starts.

Windows:

- configure application/service startup appropriately
- start before or independently of the main UI where possible

macOS:

- use an appropriate LaunchAgent/login-item mechanism

The networking/input agent should be able to run in the background without requiring the user to manually open the UI.

The UI may start alongside it, but the core agent must not depend on the UI being open.

---

## 5. Automatic peer discovery after reboot

After startup, the application must automatically scan/discover trusted peers on the local network.

Discovery must work even if:

- IP addresses changed
- machines started at different times
- some machines are still offline
- Wi-Fi reconnects slowly
- Ethernet becomes available after startup

The application should continuously listen for peer discovery announcements rather than performing only a one-time scan.

---

## 6. Automatic reconnection

Trusted peers must reconnect automatically.

Example:

```text
PC1 boots
   ↓
starts InputShare agent
   ↓
discovers PC2
   ↓
recognizes machine ID
   ↓
authenticates trusted peer
   ↓
establishes connection
   ↓
synchronizes workspace state
```

No user interaction should be required.

The same applies when PC2 starts later.

---

## 7. Partial workspace availability

The workspace must function correctly when only some machines are online.

Example:

```text
Mac       ONLINE
PC1       ONLINE
PC2       OFFLINE
```

The application must continue operating normally between Mac and PC1.

The fact that PC2 is offline must not cause errors, crashes, or unnecessary reconnection loops.

When PC2 becomes available, it should automatically rejoin.

---

## 8. Cursor routing when a machine is offline

The topology should remain persistent even when a machine is offline.

However, the cursor must never become trapped on or transferred to an unavailable machine.

Example:

```text
PC1 ───── PC2
          OFFLINE
```

If the cursor reaches the boundary leading to PC2 while PC2 is offline, the application must prevent the cursor from being transferred to PC2.

The behavior should be configurable, but a sensible default is:

- treat offline machines as temporarily non-existent for cursor routing
- preserve the configured topology
- restore the route automatically when the machine reconnects

---

## 9. State synchronization after reconnection

When a machine reconnects, the peers must synchronize all relevant state.

At minimum:

- workspace topology
- machine metadata
- display configuration
- input-device assignments
- connection state
- clipboard state where appropriate
- configuration revision

Do not assume the reconnecting machine has the latest state.

Implement a synchronization mechanism using:

- persistent configuration revision/version
- machine IDs
- update timestamps or monotonic revisions
- deterministic conflict resolution

The system must converge to the same workspace state across all trusted machines.

---

## 10. Real-time configuration changes

If the user modifies the workspace while multiple machines are online, the change must propagate immediately.

Example:

```text
User moves Mac:

LEFT OF PC1
     ↓
BELOW PC1
```

All connected machines must receive and apply the new topology immediately.

The updated configuration must also be persisted locally.

Therefore, if all machines are subsequently rebooted, they must recover the new layout rather than the old one.

---

## 11. Configuration ownership and synchronization

The workspace should conceptually be a shared distributed configuration rather than three unrelated local configurations.

Each configuration change should contain enough metadata to determine:

- who generated it
- which workspace it belongs to
- configuration revision
- previous revision
- timestamp
- affected objects

Example conceptual structure:

```text
WorkspaceUpdate {
    workspace_id
    revision
    origin_machine_id
    timestamp
    operation
}
```

Avoid blindly overwriting newer state with stale state after reconnection.

---

## 12. Workspace persistence format

Use a robust local persistence mechanism.

A simple human-readable format such as JSON/TOML may be used initially, or a small embedded database if the architecture benefits from it.

The configuration should be stored locally on every machine.

Example conceptual configuration:

```json
{
  "workspace_id": "workspace-01",
  "machines": [
    {
      "id": "mac-uuid",
      "name": "Mac",
      "position": {
        "x": -1920,
        "y": 0
      }
    },
    {
      "id": "pc1-uuid",
      "name": "PC1",
      "position": {
        "x": 0,
        "y": 0
      }
    },
    {
      "id": "pc2-uuid",
      "name": "PC2",
      "position": {
        "x": 0,
        "y": -1080
      }
    }
  ]
}
```

The exact schema is implementation-dependent.

---

## 13. First-launch experience

On the first installation of a new computer:

```text
Welcome to InputShare

Computer:
Workstation-02

Found existing workspace:

"My Desk"

PC1  ● Online
Mac  ● Online

[Join workspace]
```

After the computer has been paired once, future launches should require no interaction.

---

## 14. Reinstall behavior

If the application is completely uninstalled and installed again, treat this as a new machine by default.

Do not automatically assume that a new installation is the same trusted machine.

However, provide an explicit mechanism for restoring/importing a previous configuration if desired.

---

## 15. UI representation

The main UI should clearly distinguish:

### Online

```text
● PC1
```

### Offline but known

```text
○ PC2
   Offline
```

### Connecting

```text
◌ PC2
   Connecting...
```

### Unknown discovered machine

```text
? PC3
   New computer discovered
   [Pair]
```

The topology should remain visible regardless of machine availability.

---

# Critical requirement

A reboot must never reset the workspace.

The intended user experience is:

> Configure the workspace once. Install the application on the computers once. Pair them once. From then on, the computers should behave as a persistent shared workstation every time they boot.

The user should be able to turn all three computers off, turn them back on hours or days later, and have them automatically rediscover each other, authenticate, reconnect, synchronize their state, restore the configured topology, and resume input sharing without manual intervention.