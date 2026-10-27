#!/bin/sh
# Overlay a writable upper onto the live image's deployment state.
#
# bootc's `bootc-root-setup.service` mounts the deployment's `/var` from
# `state/deploy/<D>/var`, which on a disk install is a writable subvolume. Live
# media is read-only, so without this upper every service that writes under
# `/var` fails. Only `state/` is overlaid: the composefs repository beside it
# must stay on a real filesystem, since mounting an EROFS image through
# overlayfs fails with ENOTBLK.
. /lib/dracut-lib.sh

fail() {
    echo "katsu-composefs-state: $*" >&2
    exit 1
}

[ -d /sysroot/state ] || fail 'No deployment state in the live image'

mkdir -p /run/katsu/state
mount -t tmpfs -o mode=0755 tmpfs /run/katsu/state || fail 'Cannot mount state tmpfs'
mkdir -p /run/katsu/state/upper /run/katsu/state/work
mount -t overlay overlay \
    -o lowerdir=/sysroot/state,upperdir=/run/katsu/state/upper,workdir=/run/katsu/state/work \
    /sysroot/state \
    || fail 'Cannot overlay the deployment state'
echo "katsu-composefs-state: deployment state is writable"