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

cp -P /usr/share/uboot/rpi_3/u-boot.bin /boot/efi/rpi3-u-boot.bin
cp -P /usr/share/uboot/rpi_4/u-boot.bin /boot/efi/rpi4-u-boot.bin
rm -f /var/lib/systemd/random-seed
rm -f /etc/NetworkManager/system-connections/*.nmconnection
dnf -y remove dracut-config-generic

rm -f /etc/machine-id
touch /etc/machine-id

rm -f /var/lib/rpm/__db*

echo "Fixing SELinux labels"

fixfiles -vRa restore


dnf up -y # for downloading keys
