#!/bin/bash -x

systemctl disable systemd-networkd-wait-online systemd-networkd systemd-networkd.socket

systemctl set-default graphical.target
