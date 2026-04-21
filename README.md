<div align="center">

# 🪐 SpaceOrb V7.6 Node

**Autonomous Orbital Edge Daemon & Space Data Center Brain**

[![Rust](https://img.shields.io/badge/Rust-1.70+-orange.svg?style=for-the-badge&logo=rust)](https://rust-lang.org)
[![Python](https://img.shields.io/badge/Python-3.11+-blue.svg?style=for-the-badge&logo=python)](https://python.org)
[![Next.js](https://img.shields.io/badge/Next.js-14-black.svg?style=for-the-badge&logo=next.js)](https://nextjs.org)
[![Zenoh](https://img.shields.io/badge/DTN-Eclipse%20Zenoh-purple.svg?style=for-the-badge)](https://zenoh.io)
[![Status](https://img.shields.io/badge/Status-Zero%20Anomaly-success.svg?style=for-the-badge)](#)

*Deterministic survival of telemetry and AI-processed data in environments with zero human intervention, random power failures, and 90-minute communication blackouts.*

</div>

## 🌌 Overview

**SpaceOrb** is an aerospace-grade data management system designed for a Space Data Center. It provides rock-solid fault tolerance and data survival mechanisms for off-world computing nodes, deployed directly on resource-constrained hardware like the Raspberry Pi 5.

SpaceOrb strictly adheres to a "Zero-Anomaly" architecture, ensuring data persistence, smart transmission prioritization, and hardware limits are respected at all times, no matter what part of the orbit the node is in.

## 🚀 Key Features

- **🛡️ Secure Vaulting**: Real-time encryption (AES-256-GCM), compression (Zstd), and hashing (SHA-256) of all telemetry data. Every file write achieves hardware-level crash consistency.
- **⚡ Dynamic Prioritization ($P_{Score}$)**: Proprietary algorithm utilizing a Binary Heap to dynamically sort the transmission queue. High-value anomalies bypass routine data and jump to the front of the queue.
- **🧠 Sandboxed AI Compute**: Integrates YOLOv8 vision inference executed inside a kernel-hardened Linux cgroup container. Prevents rogue ML models from triggering Out-Of-Memory (OOM) panics or crashing the main node.
- **📡 Delay-Tolerant Networking (DTN)**: Powered by Eclipse Zenoh (QUIC) + RocksDB. Gracefully handles 10-minute ground-station passes and seamlessly resumes interrupted streams without data loss.
- **🔋 Orbital Power Awareness**: Autonomous power state management adjusting CPU workloads (via DBus/cgroups) by simulating orbital solar cycles (solar exposure vs. eclipse) to conserve battery.

## 🏗️ Architecture

The repository isolates critical paths into three completely isolated subsystems:

```mermaid
graph TD;
    subgraph Ground Station
        MC[Next.js Mission Control]
    end

    subgraph Orbital Node
        MC_Core[Rust Supervisor (mission-core)]
        MA_AI[Python AI Sandbox (mission-ai)]
        Vault[(Secure Dual USB Storage)]
    end

    MA_AI -- IPC Airgap (UDS) --> MC_Core
    MC_Core -- Atomic Storage --> Vault
    MC_Core <== DTN (Eclipse Zenoh w/ QUIC) ==> MC
```

### Directory Structure
- [`mission-core/`](./mission-core): **Indestructible Root Supervisor** (Rust/Tokio)  
  Handles atomic IO, hardware vaulting, priority queuing, and DTN.
- [`mission-ai/`](./mission-ai): **Sandboxed AI Compute** (Python/Ultralytics)  
  Runs YOLOv8 AI models confined within a rigid memory and CPU cgroup budget.
- [`mission-control/`](./mission-control): **High-Signal Dashboard** (Next.js)  
  Binary proxying and operations UI for ground station connections.
- [`scripts/`](./scripts): **Hardware Hardening** (Bash)  
  Configuration scripts establishing tmpfs, cgroups limits, RTC sync, and journaling constraints.

## 🔐 The "Vault" Rule

Every single byte written to disk must survive a sudden power loss. SpaceOrb enforces an unbreakable 6-step storage pipeline:
1. **Stage**: Write raw telemetry to `/mnt/ram_shield` (tmpfs).
2. **Process**: Pipeline execution: Compress $\to$ Encrypt $\to$ Hash.
3. **Pending**: Broadcast dual writes to both USB backup drives (application-level RAID 1).
4. **Hardware Sync**: Call `fsync()` to flush OS page caches.
5. **Commit**: Perform an atomic `rename()` to `.sealed` state.
6. **Directory Lock**: Invoke `sync_all()` on the parent directory to strictly commit metadata changes.

## ⚙️ Tech Stack Summary

| Subsystem          | Technology              | Reason                                                                     |
| ------------------ | ----------------------- | -------------------------------------------------------------------------- |
| **Root Supervisor**| Rust, Tokio, Syscalls   | Memory safety, zero-cost abstractions, robust multi-threaded IO.           |
| **Networking**     | Eclipse Zenoh, QUIC     | Native DTN capability; built-for-edge protocol to bridge space and ground. |
| **AI Sandbox**     | Python, YOLOv8, cgroups | Rapid deployment of robust vision inference cleanly walled off from OS.    |
| **Ground Station** | Next.js, WebSockets     | Efficient binary proxies and real-time interactive dashboards.             |

## 🛠️ Usage & Setup

For localized deployment on Pi 5 or development machines:

### 1. Hardening & OS Config (Phase 0)
Establish strict limits:
```bash
./scripts/01_setup_tmpfs.sh
./scripts/02_configure_cgroups.sh
```

### 2. Startup Components
The node requires mission-core and mission-ai.

**Supervisor:**
```bash
cd mission-core
cargo run --release
```

**AI Intelligence:**
```bash
cd mission-ai
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
python3 main.py
```

## 📜 License

Internal Proprietary Use Only - SpaceOrb Team
