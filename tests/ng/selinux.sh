#!/bin/bash -x

setfiles -v -F -e /proc -e /sys -e /dev -e /bin /etc/selinux/targeted/contexts/files/file_contexts / || true
setfiles -v -F -e /proc -e /sys -e /dev /etc/selinux/targeted/contexts/files/file_contexts.bin /bin || true
