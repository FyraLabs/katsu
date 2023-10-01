#!/bin/bash -x
rm -f /var/lib/systemd/random-seed
rm -f /etc/NetworkManager/system-connections/*.nmconnection

rm -f /etc/machine-id
touch /etc/machine-id

rm -f /var/lib/rpm/__db*