oci_image := "ghcr.io/fyralabs/katsu"
oci_tag := "latest"
oci_full := oci_image + ":" + oci_tag

# reminder to run as rootful OR run in a vm

podman-build:
    podman build -t {{oci_full}} .


katsu *ARGS:
    podman \
     run --rm -it \
        --privileged \
        --cap-add=ALL \
        --cgroupns=host \
        --security-opt seccomp=unconfined \
        -v ./:/workdir:Z \
        -w /workdir \
        {{ oci_full }} \
        katsu {{ARGS}}