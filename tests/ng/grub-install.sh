#!/bin/bash
set -x
# Disable os-prober for now

echo "Disabling os-prober..."

echo "GRUB_DISABLE_OS_PROBER=true" > /etc/default/grub
grub2-mkconfig > /boot/grub2/grub.cfg
rm /etc/default/grub



# get /dev/ of /boot
bootdev=$(findmnt -n -o SOURCE /boot)

# get blkid of /boot
bootid=$(blkid -s UUID -o value $bootdev)

# heredoc for /dev/disk

cat << EOF > /boot/efi/EFI/fedora/grub.cfg
search --no-floppy --fs-uuid --set=dev $bootid
set prefix=(\$dev)/grub2

export \$prefix
configfile \$prefix/grub.cfg
EOF


# GRUB entries: set ro to rw in /boot/loader/entries/*.conf
sed -i 's/ ro\n/ rw\n/g' /boot/loader/entries/*.conf


# generate fstab
efiid=$(blkid -s UUID -o value "$(findmnt -n -o SOURCE /boot/efi)")
rootid=$(blkid -s UUID -o value "$(findmnt -n -o SOURCE /)")

cat << EOF > /etc/fstab
UUID=$efiid /boot/efi vfat umask=0077,shortname=winnt 0 2
UUID=$bootid /boot ext4 defaults 0 2
UUID=$rootid / ext4 defaults 0 0
EOF

dracut -vvv --force -N --add-drivers -a "qemu qemu-net btrfs drm convertfs"
