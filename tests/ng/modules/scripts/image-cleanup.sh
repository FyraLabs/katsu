#!/bin/bash -x

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

# set password for root to "ultramarine"

# echo "root:ultramarine" | chpasswd

# Set graphical target to default
systemctl set-default graphical.target




# if aarch64
	
arch=$(uname -m)
if [[ $arch == "aarch64" ]]; then
cp -P /usr/share/uboot/rpi_arm64/u-boot.bin /boot/efi/rpi-u-boot.bin
cp -P /usr/share/uboot/rpi_3/u-boot.bin /boot/efi/rpi3-u-boot.bin
cp -P /usr/share/uboot/rpi_4/u-boot.bin /boot/efi/rpi4-u-boot.bin
fi
rm -f /var/lib/systemd/random-seed
rm -f /etc/NetworkManager/system-connections/*.nmconnection

rm -f /etc/machine-id
touch /etc/machine-id

rm -f /var/lib/rpm/__db*