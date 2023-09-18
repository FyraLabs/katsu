set +x
echo postinst
systemctl disable systemd-networkd-wait-online
systemctl disable chronyd
# systemctl disable polkit
echo max_parallel_downloads=20 >> /etc/dnf/dnf.conf
echo defaultyes=True >> /etc/dnf/dnf.conf

# dnf5 up -y # for downloading keys
