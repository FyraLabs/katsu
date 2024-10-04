# Katsu-NG architecture

Katsu-NG (v2) is a complete rewrite of the original Katsu. It's a meta-buildsystem for operating system images, designed to be robust and flexible.

## Design goals

- **Declarative**: The user should be able to describe the desired state of the image, and Katsu should figure out how to get there.
- **Extensible**: Katsu should be able to support a wide variety of build systems and image formats.
- **Cacheable**: Katsu should be able to cache intermediate build artifacts to speed up the build process.

## Objects

Katsu-NG has a few core objects that it uses to represent the state of the image:

- **Target**: A target is a single output that Katsu is trying to build. For example, a target might be a disk image, a kernel, or a bootloader.
- 