#!/bin/sh
. /lib/dracut-lib.sh
set -e

fail() {
    echo "katsu-ostree-live: $*" >&2
    exit 1
}

label=$(getarg rd.katsu.label=) || fail 'Missing rd.katsu.label'
ostree_path=$(getarg ostree=) || fail 'Missing ostree deployment boot link'
case "$ostree_path" in
    /ostree/boot.*/*/*/[0-9]*) ;;
    *) fail "Invalid OSTree boot link: $ostree_path" ;;
esac
case "$ostree_path" in
    */../* | */./*) fail 'OSTree boot link contains traversal components' ;;
esac

mkdir -p /run/initramfs/live /run/katsu/ro /run/katsu/writable
if getargbool 0 rd.live.check; then
    checkisomd5 --verbose "/dev/disk/by-label/$label" || fail 'ISO media check failed'
fi
mount -t iso9660 -o ro "/dev/disk/by-label/$label" /run/initramfs/live \
    || fail "Cannot mount ISO with label $label"
ln -s "/dev/disk/by-label/$label" /run/initramfs/livedev
image=/run/initramfs/live/LiveOS/rootfs.img
[ -f "$image" ] || fail "Root image not found: $image"
mount -o ro,loop "$image" /run/katsu/ro || fail 'Cannot mount live root image'
[ -d /run/katsu/ro/ostree/repo ] || fail 'Live image contains no OSTree repository'
[ -L "/run/katsu/ro$ostree_path" ] || fail "Boot link is missing: $ostree_path"
deployment=$(realpath -e "/run/katsu/ro$ostree_path") || fail 'Broken OSTree boot link'
case "$deployment" in
    /run/katsu/ro/ostree/deploy/*/deploy/*) ;;
    *) fail "Boot link points outside the deployment tree: $deployment" ;;
esac
[ -x "$deployment/usr/lib/systemd/systemd" ] || fail 'Deployment contains no systemd'

mount -t tmpfs -o mode=0755 tmpfs /run/katsu/writable
need_shutdown
mkdir -p /run/katsu/writable/upper /run/katsu/writable/work
# sysroot.mount mounts the merged physical sysroot; prepare-root selects the OS.
echo "katsu-ostree-live: prepared $ostree_path with a RAM-backed writable sysroot"
