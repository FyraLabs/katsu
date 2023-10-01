# Katsu

> An experimental image builder for RPM/DNF based systems.

Katsu is a tool for building bootable images from RPM based systems. It is an alternative Lennart Poettering's [mkosi](https://github.com/systemd/mkosi) tool, designed to be robust, fast, and easy to use while still providing many output formats. It is Ultramarine Linux's new image builder from Ultramarine 39 onwards.

Katsu currently supports the following output formats:

- ISO 9660 disc images
- RAW disk images

## Why Katsu?

Katsu stemmed from our frustration with Fedora's Lorax/OSBuild toolchain. Lorax is a very complex Python application that relies on another complex Python application, Anaconda, to build images. Then on top of that uses hard-to-read Mako templates to configure the image on top.

We found it difficult to work with Lorax and Anaconda, and we wanted to build images in a more straightforward way where we can control any aspect of the image building process down to the filesystem layout. And thus, Katsu was born.

Katsu uses YAML configuration files to describe the image, with modular manifests similar to the likes of rpm-ostree. This makes it easy to read, write, and maintain Katsu configurations.

## Dependencies

- `xorriso`
- `clang-devel`
- `dracut`
- `limine` or `grub2`
- `rpm`
- `dnf` or `dnf5`
- `systemd-devel`
