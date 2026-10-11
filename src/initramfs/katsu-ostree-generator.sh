#!/bin/sh
# The ISO label is a device dependency, not the deployment's root device.
. /lib/dracut-lib.sh
getargbool 0 rd.katsu.ostree || exit 0
label=$(getarg rd.katsu.label=)
[ -n "$label" ] || exit 1
device=$(systemd-escape --path --suffix=device "/dev/disk/by-label/$label")
mkdir -p "$1/katsu-ostree-live.service.d"
cat > "$1/katsu-ostree-live.service.d/media.conf" <<EOF
[Unit]
Requires=$device
After=$device
EOF
