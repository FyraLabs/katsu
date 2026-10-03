#!/bin/sh
# Prepare the live media for a native composefs deployment.
#
# Unlike the OSTree path, root selection is not our job: bootc's own
# `bootc-root-setup.service` reads `composefs=<D>` and assembles the root from
# `/sysroot/composefs`. We only have to make the ISO the backing filesystem and
# give the repository a writable upper, then let bootc take over.
. /lib/dracut-lib.sh

fail() {
    echo "katsu-composefs-live: $*" >&2
    exit 1
}

label=$(getarg rd.katsu.label=) || fail 'Missing rd.katsu.label'
[ -n "$label" ] || fail 'Empty rd.katsu.label'

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

[ -d /run/katsu/ro/composefs ] || fail 'Live image contains no composefs repository'
# bootc-root-setup opens the deployment state even when no /etc or /var mount is
# configured, so its absence is fatal later and worth catching here.
boot_digest=$(getarg composefs=)
boot_digest=${boot_digest#\?}
[ -d "/run/katsu/ro/state/deploy/$boot_digest" ] \
    || fail "Live image has no state for deployment $boot_digest"

mount -t tmpfs -o mode=0755 tmpfs /run/katsu/writable
mkdir -p /run/katsu/writable/upper /run/katsu/writable/work
# Mounts created from read-only media must survive switch-root teardown.
need_shutdown
echo "katsu-composefs-live: prepared composefs media with $boot_digest"