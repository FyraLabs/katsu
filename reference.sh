#!/bin/bash -x

IMAGE="ghcr.io/korewachino/ucore-cappy:latest"
ROOTFS="rootfs"

# sudo rm -rf "$ROOTFS"
# sudo mkdir -p "$ROOTFS"


# let's create an ephemeral container

sudo podman pull $IMAGE

ctr=$(sudo podman create --rm "$IMAGE" /bin/bash)

sudo podman export $ctr | sudo tar -xf - -C "$ROOTFS"

# now that we got the rootfs

pushd "$ROOTFS" || exit 1

# Let's push the podman image inside there too
sudo mkdir -p "var/lib/containers/storage"
TARGET_CONTAINERS_STORAGE=$(realpath "var/lib/containers/storage")

sudo podman push "${IMAGE}" "containers-storage:[overlay@${TARGET_CONTAINERS_STORAGE}]$IMAGE" --remove-signatures

# Now we can basically do the rest as we did before with katsu