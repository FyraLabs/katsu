#!/bin/sh
. /lib/dracut-lib.sh

fail() {
    echo "katsu-ostree-mountpoints: $*" >&2
    exit 1
}

# getarg calls debug_on, which may return 1: do not invoke it under errexit.
ostree_path=$(getarg ostree=) || fail 'Missing ostree deployment boot link'
deployment=$(realpath -e "/sysroot$ostree_path") \
    || fail "Cannot resolve deployment boot link: /sysroot$ostree_path"
case "$deployment" in
    /sysroot/ostree/deploy/*/deploy/*) ;;
    *) fail "Invalid deployment after sysroot mount: $deployment" ;;
esac
mkdir -p "$deployment/sysroot" "$deployment/var" \
    || fail "Cannot create mountpoints in deployment: $deployment"
echo "katsu-ostree-mountpoints: prepared mountpoints in $deployment"
# prepare-root creates /run/ostree itself. The real-root OSTree generator
# mounts the stateroot's var from the writable physical sysroot.
