#!/bin/bash -x
# Re-generate GRUB configuration in case Katsu fails to create one during the build process
grub2-mkconfig -o /boot/grub2/grub.cfg
