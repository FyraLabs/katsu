#!/usr/bin/bash
# Boot an OSTree sysroot from Katsu live media (rd.katsu.ostree).

check() {
    # Only included on request: katsu passes `-a katsu-ostree`.
    return 255
}

depends() {
    echo ostree systemd
}

installkernel() {
    hostonly='' instmods erofs squashfs overlay loop iso9660
}

install() {
    inst_multiple mount mkdir realpath touch ln losetup systemd-escape
    inst_multiple -o checkisomd5

    inst_script "$moddir/katsu-ostree-live.sh" /usr/libexec/katsu-ostree-live
    inst_script "$moddir/katsu-ostree-mountpoints.sh" /usr/libexec/katsu-ostree-mountpoints
    inst_script "$moddir/katsu-ostree-generator.sh" \
        "$systemdutildir/system-generators/katsu-ostree-generator"
    inst_hook cmdline 99 "$moddir/katsu-ostree-cmdline.sh"

    inst_simple "$moddir/katsu-ostree-live.service" "$systemdsystemunitdir/katsu-ostree-live.service"
    inst_simple "$moddir/sysroot.mount" "$systemdsystemunitdir/sysroot.mount"
    inst_simple "$moddir/bind-mount.conf" "$systemdsystemunitdir/sysroot-usr.mount.d/katsu-live.conf"
    inst_simple "$moddir/bind-mount.conf" "$systemdsystemunitdir/sysroot-sysroot.mount.d/katsu-live.conf"
    inst_simple "$moddir/prepare-root.service.conf" \
        "$systemdsystemunitdir/ostree-prepare-root.service.d/katsu-live.conf"

    # The ostree module may already have installed the image's own copy.
    rm -f "$initdir/etc/ostree/prepare-root.conf"
    inst_simple "$moddir/prepare-root.conf" /etc/ostree/prepare-root.conf

    mkdir -p "$initdir$systemdsystemunitdir/initrd-root-fs.target.requires"
    ln_r "$systemdsystemunitdir/sysroot.mount" \
        "$systemdsystemunitdir/initrd-root-fs.target.requires/sysroot.mount"
}
