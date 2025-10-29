# Tests for building whole chroot entirely inside Docker container

This test suite is for verifying that Katsu can run entirely inside a Docker build step,
allowing base images to be built with Katsu unprivileged.

## Usage

1. Install Docker on your system.
2. Go to the root of the Katsu project.
3. Run the following command to build the Docker image and execute the test:

    ```sh
    docker build -t katsu-test -f tests/ng/docker/Dockerfile .
    docker run --rm katsu-test busybox echo "Hello from docker image!"
    ```
