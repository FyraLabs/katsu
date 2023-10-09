#!/bin/bash
echo "Hello from pre2.sh"
echo "This is an integration test for Katsu, the Ultramarine image builder."
# check if $CHROOT is set, if not, exit


echo "Testing if CHROOT is set..."
if [ -z "$CHROOT" ]; then
    echo "CHROOT is not set, exiting."
    exit 1
fi
echo "CHROOT: $CHROOT"

# check if $CHROOT is a directory, if not, exit
echo "Testing if CHROOT is a directory..."
if [ ! -d "$CHROOT" ]; then
    echo "CHROOT is not a directory, exiting."
    exit 1
fi

# check if $CHROOT is readable, if not, exit
echo "Testing if CHROOT is readable..."
if [ ! -r "$CHROOT" ]; then
    echo "CHROOT is not readable, exiting."
    exit 1
fi

# check if $CHROOT is writable, if not, exit
echo "Testing if CHROOT is writable..."
if [ ! -w "$CHROOT" ]; then
    echo "CHROOT is not writable, exiting."
    exit 1
fi



echo "Enjoy!"