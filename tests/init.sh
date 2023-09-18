#!/bin/bash -x
set -x
echo init moment

mkdir -p ./etc/yum.repos.d ./etc/dnf


cat << EOF > ./etc/yum.repos.d/terra.repo
[terra]
name=Terra \$releasever
baseurl=https://repos.fyralabs.com/terra\$releasever
type=rpm
skip_if_unavailable=True
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://repos.fyralabs.com/terra\$releasever/key.asc
enabled=1
enabled_metadata=1
metadata_expire=4h
EOF


cat << EOF > ./etc/yum.repos.d/fedora.repo
[fedora]
name=Fedora \$releasever - \$basearch
metalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-\$releasever&arch=\$basearch
enabled=1
countme=1
metadata_expire=7d
repo_gpgcheck=0
type=rpm
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-\$releasever-\$basearch
skip_if_unavailable=False

[fedora-debuginfo]
name=Fedora \$releasever - \$basearch - Debug
metalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-debug-\$releasever&arch=\$basearch
enabled=0
metadata_expire=7d
repo_gpgcheck=0
type=rpm
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-\$releasever-\$basearch
skip_if_unavailable=False

[fedora-source]
name=Fedora \$releasever - Source
metalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-source-\$releasever&arch=\$basearch
enabled=0
metadata_expire=7d
repo_gpgcheck=0
type=rpm
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-\$releasever-\$basearch
skip_if_unavailable=False
EOF


cat <<EOF > ./etc/yum.repos.d/fedora-updates.repo
[updates]
name=Fedora \$releasever - \$basearch - Updates
metalink=https://mirrors.fedoraproject.org/metalink?repo=updates-released-f\$releasever&arch=\$basearch
enabled=1
countme=1
repo_gpgcheck=0
type=rpm
gpgcheck=1
metadata_expire=6h
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-\$releasever-\$basearch
skip_if_unavailable=False

[updates-debuginfo]
name=Fedora \$releasever - \$basearch - Updates - Debug
metalink=https://mirrors.fedoraproject.org/metalink?repo=updates-released-debug-f\$releasever&arch=\$basearch
enabled=0
repo_gpgcheck=0
type=rpm
gpgcheck=1
metadata_expire=6h
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-\$releasever-\$basearch
skip_if_unavailable=False

[updates-source]
name=Fedora \$releasever - Updates Source
metalink=https://mirrors.fedoraproject.org/metalink?repo=updates-released-source-f\$releasever&arch=\$basearch
enabled=0
repo_gpgcheck=0
type=rpm
gpgcheck=1
metadata_expire=6h
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-\$releasever-\$basearch
skip_if_unavailable=False
EOF


cat <<EOF > ./etc/yum.repos.d/ultramarine.repo
[ultramarine]
name=Ultramarine Linux \$releasever
baseurl=https://repos.fyralabs.com/um\$releasever
metadata_expire=6h
type=rpm
skip_if_unavailable=True
gpgcheck=1
gpgkey=https://repos.fyralabs.com/um\$releasever/key.asc
repo_gpgcheck=1
enabled=1
enabled_metadata=1
EOF


cat <<EOF ./etc/dnf/dnf.conf

[main]
gpgcheck=True
installonly_limit=3
clean_requirements_on_remove=True
best=False
skip_if_unavailable=True
defaultyes=True
max_parallel_downloads=20
countme=False
install_weak_deps=False
EOF

echo "bai"
