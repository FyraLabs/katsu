#!/bin/bash -x

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
sed -i 's/ ro/ rw/g' /boot/loader/entries/*.conf
