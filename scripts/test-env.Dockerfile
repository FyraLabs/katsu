FROM ghcr.io/terrapkg/builder:f42

RUN --mount=type=bind,target=/var/cache/dnf \
    dnf install -y \
    xorriso \
    rpm \
    limine \
    systemd \
    btrfs-progs \
    e2fsprogs \
    xfsprogs \
    dosfstools \
    grub2 \
    parted \
    gdisk \
    util-linux-core \
    systemd-container \
    grub2-efi \
    uboot-images-armv8 \
    uboot-tools \
    rustc \
    qemu-user-static-aarch64 \
    qemu-user-binfmt \
    qemu-kvm \
    qemu-img \
    cargo \
    systemd-devel \
    mkpasswd \
    clang-devel \
    moby-engine \
    squashfs-tools \
    erofs-utils \
    grub2-tools \
    grub2-tools-extra \
    isomd5sum \
    dnf5 \
    podman

