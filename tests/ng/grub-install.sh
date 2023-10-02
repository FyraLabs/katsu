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


dracut -vfN --add-drivers "virtio virtio_blk virtio_scsi xchi_pci mmc" --regenerate-all