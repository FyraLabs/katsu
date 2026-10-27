#!/usr/bin/bash
# Expose Katsu live media as /sysroot for bootc-root-setup (rd.katsu.composefs).

check() {
    # Only included on request: katsu passes `-a katsu-composefs`.
    return 255
}

depends() {
    echo bootc systemd
}

installkernel() {
    hostonly='' instmods erofs squashfs overlay loop iso9660
}

install() {
    inst_multiple mount mkdir realpath touch ln losetup systemd-escape
    inst_multiple -o checkisomd5

    inst_script "$moddir/katsu-composefs-live.sh" /usr/libexec/katsu-composefs-live
    inst_script "$moddir/katsu-composefs-state.sh" /usr/libexec/katsu-composefs-state
    inst_script "$moddir/katsu-composefs-generator.sh" \
        "$systemdutildir/system-generators/katsu-composefs-generator"

    inst_simple "$moddir/katsu-composefs-live.service" \
        "$systemdsystemunitdir/katsu-composefs-live.service"
    inst_simple "$moddir/katsu-composefs-state.service" \
        "$systemdsystemunitdir/katsu-composefs-state.service"
    inst_simple "$moddir/sysroot.mount" "$systemdsystemunitdir/sysroot.mount"

    mkdir -p "$initdir$systemdsystemunitdir/initrd-root-fs.target.requires"
    ln_r "$systemdsystemunitdir/sysroot.mount" \
        "$systemdsystemunitdir/initrd-root-fs.target.requires/sysroot.mount"
    # Before= in the unit only orders; the state overlay also has to be pulled in.
    mkdir -p "$initdir$systemdsystemunitdir/bootc-root-setup.service.wants"
    ln_r "$systemdsystemunitdir/katsu-composefs-state.service" \
        "$systemdsystemunitdir/bootc-root-setup.service.wants/katsu-composefs-state.service"
}
