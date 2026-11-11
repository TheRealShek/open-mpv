FROM fedora:44
RUN dnf --setopt=install_weak_deps=False -y install \
      qemu-system-x86 qemu-ui-sdl qemu-device-display-virtio-vga-gl mesa-dri-drivers mesa-libGL \
      xorg-x11-server-Xvfb e2fsprogs python3 git docker-cli docker-buildx \
    && rpm -qa | sort > /runner-packages.txt \
    && dnf clean all
