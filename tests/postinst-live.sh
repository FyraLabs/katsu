#!/bin/bash -x
echo postinst
systemctl disable systemd-networkd-wait-online systemd-networkd systemd-networkd.socket
systemctl disable chronyd
# systemctl disable polkit
# Add polkitd user manually, weird hack because preinstall script doesn't run
cat << EOF > /usr/lib/sysusers.d/extras.conf
# This is a really weird hack
#Type  Name    ID   Argument
g      polkitd 114  
u      polkitd 114:114 "User for polkitd" - -

g      rpc     32
u      rpc     32:32  "Rpcbind Daemon" - -
EOF

echo max_parallel_downloads=20 >> /etc/dnf/dnf.conf
echo defaultyes=True >> /etc/dnf/dnf.conf

systemd-sysusers
# dnf -y remove dracut-config-generic

# dracut -vvv --force --regenerate-all -N

rm -f /etc/machine-id
touch /etc/machine-id

rm -f /var/lib/rpm/__db*

echo "Fixing SELinux labels"

setfiles -v -F -e /proc -e /sys -e /dev -e /bin /etc/selinux/targeted/contexts/files/file_contexts /
setfiles -v -F -e /proc -e /sys -e /dev -e /etc/selinux/targeted/contexts/files/file_contexts.bin /bin

