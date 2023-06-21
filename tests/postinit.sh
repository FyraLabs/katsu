echo postinit moment
dnf install -y dnf-plugins-core
dnf config-manager --add-repo='https://github.com/terrapkg/subatomic-repos/raw/main/terra38.repo'
