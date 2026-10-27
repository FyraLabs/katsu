#!/bin/sh
# The ISO label is a device dependency, not the deployment's root device.
# Without this the live service races udev and /dev/disk/by-label/<label> does
# not exist yet, so the ISO mount fails with "Can't lookup blockdev".
. /lib/dracut-lib.sh
getargbool 0 rd.katsu.composefs || exit 0
label=$(getarg rd.katsu.label=)
[ -n "$label" ] || exit 1
device=$(systemd-escape --path --suffix=device "/dev/disk/by-label/$label")
mkdir -p "$1/katsu-composefs-live.service.d"
cat > "$1/katsu-composefs-live.service.d/media.conf" <<EOF
[Unit]
Requires=$device
After=$device
EOF