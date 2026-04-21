#!/usr/bin/env bash
# =============================================================================
# SpaceOrb V7.6 — Raspberry Pi 5 OS Hardening Script
# =============================================================================
#
# This script configures the Pi 5 for the SpaceOrb orbital edge daemon:
# 1. Mounts RAM shield tmpfs (1GB)
# 2. Configures dual USB ext4 vaults (journaling disabled)
# 3. Installs systemd services
# 4. Creates required users/groups
#
# Reference: SPACEORB_CORE_SPEC.txt §3.2, §5
#
# Usage: sudo bash setup_pi5.sh
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*"; }

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------

if [[ $EUID -ne 0 ]]; then
    log_error "This script must be run as root (sudo)."
    exit 1
fi

log_info "=== SpaceOrb V7.6 Pi 5 Setup ==="
log_info "Starting OS hardening..."

# ---------------------------------------------------------------------------
# 1. Create system user for AI sandbox
# ---------------------------------------------------------------------------

if ! id "mission-ai" &>/dev/null; then
    useradd --system --no-create-home --shell /usr/sbin/nologin mission-ai
    log_info "Created system user: mission-ai"
else
    log_info "User mission-ai already exists"
fi

# ---------------------------------------------------------------------------
# 2. Mount RAM Shield (1GB tmpfs)
# ---------------------------------------------------------------------------

RAM_SHIELD="/mnt/ram_shield"
mkdir -p "${RAM_SHIELD}"

# Add to fstab if not present
if ! grep -q "ram_shield" /etc/fstab; then
    echo "# SpaceOrb V7.6 — RAM Shield (1GB tmpfs)" >> /etc/fstab
    echo "tmpfs ${RAM_SHIELD} tmpfs nodev,nosuid,noexec,size=1G 0 0" >> /etc/fstab
    log_info "Added RAM shield to /etc/fstab"
fi

# Mount now
if ! mountpoint -q "${RAM_SHIELD}"; then
    mount "${RAM_SHIELD}"
    log_info "Mounted RAM shield at ${RAM_SHIELD} (1GB tmpfs)"
else
    log_info "RAM shield already mounted"
fi

# Set permissions
chmod 1777 "${RAM_SHIELD}"

# ---------------------------------------------------------------------------
# 3. Configure Dual USB Vaults (ext4, no journaling)
# ---------------------------------------------------------------------------

USB_PRIMARY="/mnt/usb_primary"
USB_MIRROR="/mnt/usb_mirror"

mkdir -p "${USB_PRIMARY}" "${USB_MIRROR}"

# Detect USB devices (common Pi 5 USB 3.1 paths)
USB_DEV1="${USB_DEV1:-/dev/sda1}"
USB_DEV2="${USB_DEV2:-/dev/sdb1}"

log_info "USB Primary device: ${USB_DEV1}"
log_info "USB Mirror  device: ${USB_DEV2}"

# Disable journaling on ext4 (requires unmount first)
for dev in "${USB_DEV1}" "${USB_DEV2}"; do
    if [ -b "${dev}" ]; then
        # Check if currently mounted and unmount
        mount_point=$(findmnt -n -o TARGET "${dev}" 2>/dev/null || true)
        if [ -n "${mount_point}" ]; then
            umount "${dev}" 2>/dev/null || true
        fi

        # Disable journaling
        tune2fs -O ^has_journal "${dev}" 2>/dev/null && \
            log_info "Disabled journaling on ${dev}" || \
            log_warn "Could not disable journaling on ${dev} (may already be disabled)"

        # Run fsck
        e2fsck -fy "${dev}" 2>/dev/null || true
    else
        log_warn "Device ${dev} not found — skipping"
    fi
done

# Add to fstab with noatime, data=ordered, passno=2 (auto-repair)
if ! grep -q "usb_primary" /etc/fstab; then
    cat >> /etc/fstab << EOF

# SpaceOrb V7.6 — Dual USB Vaults (ext4, no journal)
${USB_DEV1} ${USB_PRIMARY} ext4 noatime,data=ordered,nofail 0 2
${USB_DEV2} ${USB_MIRROR}  ext4 noatime,data=ordered,nofail 0 2
EOF
    log_info "Added USB vaults to /etc/fstab (passno=2 for auto-repair)"
fi

# Mount now
mount -a 2>/dev/null || log_warn "Some mounts may have failed (check /etc/fstab)"
log_info "USB vaults mounted"

# ---------------------------------------------------------------------------
# 4. Create spool directory for DTN
# ---------------------------------------------------------------------------

SPOOL_DIR="/var/lib/spaceorb/spool"
mkdir -p "${SPOOL_DIR}"
chown root:root "${SPOOL_DIR}"
chmod 755 "${SPOOL_DIR}"
log_info "Created DTN spool directory: ${SPOOL_DIR}"

# ---------------------------------------------------------------------------
# 5. Install systemd service files
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "${SCRIPT_DIR}")"

# Install mission-ai service
if [ -f "${SCRIPT_DIR}/mission-ai.service" ]; then
    cp "${SCRIPT_DIR}/mission-ai.service" /etc/systemd/system/mission-ai.service
    log_info "Installed mission-ai.service"
elif [ -f "${REPO_ROOT}/mission-ai/sandbox.service" ]; then
    cp "${REPO_ROOT}/mission-ai/sandbox.service" /etc/systemd/system/mission-ai.service
    log_info "Installed mission-ai.service from sandbox.service"
fi

# Reload systemd
systemctl daemon-reload
log_info "Systemd daemon reloaded"

# ---------------------------------------------------------------------------
# 6. Kernel parameters (optional, for performance)
# ---------------------------------------------------------------------------

# Increase max file descriptors
if ! grep -q "spaceorb" /etc/security/limits.conf 2>/dev/null; then
    cat >> /etc/security/limits.conf << EOF

# SpaceOrb V7.6 — Increased file descriptor limits
* soft nofile 65536
* hard nofile 65536
EOF
    log_info "Set file descriptor limits"
fi

# Reduce swappiness (we manage our own memory budget)
sysctl -w vm.swappiness=10 2>/dev/null || true

if ! grep -q "vm.swappiness" /etc/sysctl.d/99-spaceorb.conf 2>/dev/null; then
    mkdir -p /etc/sysctl.d/
    echo "vm.swappiness=10" > /etc/sysctl.d/99-spaceorb.conf
    log_info "Set vm.swappiness=10"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
log_info "=== SpaceOrb V7.6 Setup Complete ==="
echo ""
echo "  RAM Shield:    ${RAM_SHIELD} (1GB tmpfs)"
echo "  USB Primary:   ${USB_PRIMARY} (${USB_DEV1})"
echo "  USB Mirror:    ${USB_MIRROR} (${USB_DEV2})"
echo "  DTN Spool:     ${SPOOL_DIR}"
echo "  AI Service:    mission-ai.service"
echo ""
echo "  Next steps:"
echo "    1. Build mission-core:  cd mission-core && cargo build --release"
echo "    2. Setup AI venv:       cd mission-ai && python3 -m venv venv && source venv/bin/activate && pip install -r requirements.txt"
echo "    3. Enable services:     systemctl enable --now mission-ai"
echo "    4. Run supervisor:      ./mission-core/target/release/mission-core"
echo ""
