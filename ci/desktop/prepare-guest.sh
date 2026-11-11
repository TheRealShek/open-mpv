#!/usr/bin/env bash
# Build a bootable, disposable guest. Never run this on a workstation.
set -euo pipefail
desktop=$1
mkdir -p /opt/open-mpv
case "$desktop" in
  gnome)
    dnf --setopt=install_weak_deps=False -y install \
      kernel-core kernel-modules dracut systemd systemd-pam pam authselect shadow-utils util-linux \
      gnome-shell gnome-settings-daemon dbus-daemon mesa-dri-drivers mesa-libEGL \
      gtk4 python3-gobject python3-cairo python3-pillow adwaita-icon-theme
    if [ "${PROBE_ONLY:-0}" != 1 ]; then
      dnf --setopt=install_weak_deps=False -y install \
        cargo rust gcc pkgconf-pkg-config gtk4-devel glycin-devel gstreamer1-devel \
        ImageMagick glycin-loaders gstreamer1-plugin-gtk4 gstreamer1-plugins-good ffmpeg-free
    fi
    authselect select local --force
    kernel=$(find /usr/lib/modules -mindepth 1 -maxdepth 1 -printf '%f\n' | sort -V | tail -1)
    dracut --no-hostonly --add-drivers 'virtio_pci virtio_blk virtio_gpu 9p 9pnet_virtio' \
      --force /boot/initramfs-ci.img "$kernel"
    cp "/usr/lib/modules/$kernel/vmlinuz" /boot/vmlinuz-ci
    rpm -qa | sort > /opt/open-mpv/packages.txt
    dnf clean all
    ;;
  hyprland)
    pacman -Syu --noconfirm --needed \
      linux mkinitcpio systemd-sysvcompat shadow util-linux dbus \
      hyprland mesa gtk4 grim python-gobject python-cairo python-pillow
    if [ "${PROBE_ONLY:-0}" != 1 ]; then
      pacman -S --noconfirm --needed rust gcc pkgconf imagemagick glycin \
        gst-plugin-gtk4 gst-plugins-good ffmpeg
    fi
    printf '%s\n' 'MODULES=(virtio_pci virtio_blk virtio_gpu 9p 9pnet_virtio)' \
      'HOOKS=(base udev modconf block filesystems)' > /tmp/mkinitcpio-ci.conf
    mkinitcpio -k /boot/vmlinuz-linux -c /tmp/mkinitcpio-ci.conf -g /boot/initramfs-ci.img
    cp /boot/vmlinuz-linux /boot/vmlinuz-ci
    pacman -Q > /opt/open-mpv/packages.txt
    pacman -Scc --noconfirm
    ;;
  *) echo "Unknown desktop: $desktop" >&2; exit 2 ;;
esac

useradd --create-home --shell /bin/bash ci
passwd -d ci
install -d -o ci -g ci /opt/open-mpv
truncate -s 0 /etc/machine-id
printf '%s\n' "$desktop" > /etc/open-mpv-ci-desktop
mkdir -p /artifacts /etc/systemd/system/getty@tty1.service.d
printf '%s\n' 'artifacts /artifacts 9p trans=virtio,version=9p2000.L,msize=262144 0 0' >> /etc/fstab
cat > /etc/systemd/system/getty@tty1.service.d/ci.conf <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin ci --noclear %I linux
EOF
cat > /home/ci/.bash_profile <<'EOF'
if [ "$(tty)" = /dev/tty1 ]; then
  cd /opt/open-mpv
  exec ci/desktop/session.sh >> /artifacts/session.log 2>&1
fi
EOF
systemctl enable getty@tty1.service
systemctl set-default multi-user.target
# Keep the distribution's relative init link: the BusyBox initramfs checks it
# under /new_root before chroot, where an absolute link resolves incorrectly.
test -x /sbin/init
