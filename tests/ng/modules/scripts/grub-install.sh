#!/bin/bash
set -x
# Re-generate GRUB configuration in case Katsu fails to create one during the build process
grub2-mkconfig -o /boot/grub2/grub.cfg

# get /dev/ of /boot, or / if /boot is not a separate partition
function find_bootdev {
    # try findmnt /boot
    if findmnt -n -o SOURCE /boot; then
        bootdev=$(findmnt -n -o SOURCE /boot)
    else
        bootdev=$(findmnt -n -o SOURCE /)
    fi
}


find_bootdev
# get blkid of /boot
bootid=$(blkid -s UUID -o value $bootdev)

cat << EOF > /boot/efi/EFI/fedora/grub.cfg
search --no-floppy --fs-uuid --set=dev $bootid
set prefix=(\$dev)/grub2

export \$prefix
configfile \$prefix/grub.cfg
EOF

# edit ro to rw in all entries

sed -i 's/ ro  / rw  /g' /boot/loader/entries/*.conf


dracut -vfN --add-drivers "virtio virtio_blk virtio_scsi xchi_pci mmc" --regenerate-all