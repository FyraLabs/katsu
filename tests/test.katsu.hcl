// This is a draft of the custom HCL configuration format for Katsu
// Extensions should be .katsu.hcl'
// 
// non-blocks in the top level are considered variables, however if you want to be more explicit and
// type them, you can use the `var` keyword

/*
                                                  +---------------------------+                                          
                                                  |                           |                                          
                                                  |                           |                                          
                                                  |                           |                                          
                                                  |       base                |                                          
                                                  |                           |                                          
                                                  |                           +------------------------------+           
                                                  |                           |                              |           
                                                  |                           |                              |           
                                      +-----------+-------+-------------------+------+                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |                          |                       |           
                                      |                   |               +----------v----------+  +---------v----------+
                             +--------v---------+ +-------v--------+      |                     |  |                    |
                             |                  | |                |      |                     |  |                    |
                             |                  | |                |      |                     |  |                    |
                             |                  | |                |      |                     |  |                    |
                             |    desktop       | |  minimal       |      |     oci             |  |   minimal disk     |
                             |                  | |     squash     |      |                     |  |                    |
                             |                  | |                |      |                     |  |                    |
                             |                  | |                |      |                     |  |                    |
                             |                  | |                |      |                     |  |                    |
                  +----------+-------+----------+-+----------------+      |                     |  |                    |
                  |                  |            |                       +---------------------+  +--------------------+
                  |                  |            |                                                                      
                  |                  |            |                                                                      
          +-------+-------+  +-------v------+  +--v-----------+                                                          
          |       v       |  |              |  |              |                                                          
          |               |  |              |  |              |                                                          
          |               |  |              |  | Spin 3       |                                                          
          | Spin 1        |  |  Spin 2      |  |              |                                                          
          |               |  |              |  |              |                                                          
          |               |  |              |  |              |                                                          
          |         +-+   |  |              |  |              |                                                          
        +-+---------+-+---+  +---+---------++  +--+---------+-+                                                          
        |             |          |         |      |         |                                                            
+-------v---+ +-------v----+ +---+---++----v-+ +--+---++----+-+                                                          
|           | |            | |   v   ||      | |  v   ||    v |                                                          
|           | |            | |squash ||disk  | | sqsh || disk |                                                          
| squash    | |            | |       ||      | |      ||      |                                                          
|           | | disk       | |       ||      | +------++------+                                                          
|           | |            | ++------++------+ |      |                                                                  
|           | |            |  |      |         | iso  |                                                                  
++----------+ +------------+  | iso  |         |      |                                                                  
 +----------+                 |      |         +------+                                                                  
 |          |                 +------+                                                                                   
 |          |                                                                                                            
 |  iso     |                                                                                                            
 |          |                                                                                                            
 |          |                                                                                                            
 +----------+                                                                                                            
 */

var "foo" {
    type = "string"
    default = "bar"
}

var "dnf_releasever" {
    default = 40
}

// import a subdirectory by doing `import {}`
# module {
#     source = "./module"
# }

// you should reference data from a submodule by doing module.data_name

// in Rust code we can keep all of this in the heap, it's not a big deal
// (Box<PkgList>) or something like that, Arc maybe?

pkg_list "dnf" "core" {
    default = [
        "@core",
        "kernel-*" // you can glob here because DNF lets you do that
    ]
    arch_specific = {
        "x86_64" = [
            "grub2-efi-x64",
            "grub2-efi-x64-modules"
        ]
        "aarch64" = [
            "grub2-efi-aarch64",
            "grub2-efi-aarch64-modules"
        ]
    }
    exclude_arch = {
        "x86_64" = [
            "grub2-efi-aarch64",
            "grub2-efi-aarch64-modules"
        ]
        "aarch64" = [
            "grub2-efi-x64",
            "grub2-efi-x64-modules"
        ]
    }
}
// Output block
// 
// There will be an `output.output_file` key that will be evaluated to the output file
// 
// So one could create a new `squashfs` output that bootstraps the system and packs it into a squashfs
// 
// Then do an `iso` output that copies output.squashfs to root
// 
// First field is the output type, second field is the output name
// 
// For "disk" and "iso" types, you may mount the output file to a directory,
// 
// For "iso" and disk types you may want to put custom trees as URIs like `$part:/path_to_file` or something
// if you are not expecting to make it a full root filesystem
// 
// Planned types:
// - disk - Partitioned disk images with optional mountpoints for post-processing and scripting
// - dir - Outputs an entire directory tree, useful as a pipeline to further outputs
// - squashfs - Squashfs filesystems, Can be bootstrapped standalone or from a dir output
// - iso - ISO images, can be bootstrapped from a squashfs or dir output, Needs manual layout setup (we need helpers for this)
// - tar - Tarballs, can be bootstrapped from a dir output
// - oci - OCI base image, works similarly to a `tar` output, but writes additional OCI metadata JSONs
// 
// `disk` outputs should always have its own partition layout, and should be used as the final pass,
// we will not be supporting disk bootstrap types
output "disk" "xfce" {
    
    // dev note: the output object is gonna be crazy, we would need like a million optional fields with validation...
    
    // Method to bootstrap the system.
    // Can be a package manager like `dnf`, or from a docker image, tarball or squashfs

    
    // Planned possible bootstrap methods:
    // - oci - Copies data from an OCI image using `podman export` or `skopeo` or something, behaviour will be similar to a `dir` or `tar` input
    // - tar - Extracts a tarball to the tree
    // - squashfs - unsquash the squashfs to the tree
    // - dir - Copy files from a directory to the tree
    // - dnf - Install packages from a package list
    // 
    bootstrap_method = "dnf"
    dnf {
        // there will be an `arch` key here, implied by the host environment or explicitly set by an argument
        
        // import package lists from this
        package_lists = [
            // pkg_list.dnf.core,
            // pkg_list.dnf.xfce
            // module.foo.pkg_list.dnf.bar
        ]
        
        // or just list packages here
        
        packages {
            default = [
                
            ]
            exclude = []
            arch_specific = {
                "x86_64" = [
                    
                ]
                "aarch64" = [
                    
                ]
            }
            exclude_arch = {
                "x86_64" = [
                    
                ]
                "aarch64" = [
                    
                ]
            }   
        }
    }
    partition_layout {
        // layout here, partno is sorted by order of appearance
        // the table here will be then passed to a respective partitioning table
        // structure (i.e Partition for disks, Iso9660, etc.)
        partition {
            label = "ESP"
            type = "esp" // or guid = "$guid"
            filesystem = "fat32"
            size = "512M"
            mountpoint = "/boot/efi"
        }
        partition {
            label = "boot"
            type = "xbootldr"
            filesystem = "ext4"
            size = "2G"
            mountpoint = "/boot"
        }
        partition {
            type = "cros-kernel"
            // optional copy_blocks directive to `dd` blocks to the filesystem
            // instead of copying files to the filesystem
            size = "16M"
            // copy_blocks takes source file as abspath, or relative to the current manifest
            copy_blocks = "/usr/share/submarine/submarine.kpart"
        }
        partition {
            label = "root"
            type = "root" // will be inferred from system arch
            filesystem = "ext4"
            size = "rest"
            mountpoint = "/"
        }
    }
    // optional copy_files directive to copy files to the filesystem, will be relative to
    // root partition or something like systemd-repart?
    // 
    // File copying should be done after bootstrapping the system
    
    copy {
        // Path should be relative to the root partition
        source = "./somefile"
        // Destination should be relative to the root of the target directory
        // It can be a full root filesystem or just a normal directory tree (Accomodating a custom file layout
        // possibly may want for a live image where you can place grub configs and squashfs images in the root)
        destination = "/somefile"
    }
    
    // Sort execution of scripts by priority or placement, whichever comes first
    // Priority should be integer, the lower the number, the earlier it is executed
    // 
    // note: needs custom sorting function to sort by index or priority, selecting priority over index if exists
    
    // index will always be defined, so it will be used as a fallback if priority is not defined
    script {
        id = "grub"
        // source = file("./grub.cfg")
        priority = 10
    }
}


// If you do an output based on an existing rootfs, you can even do multiple passes :3
// So say, new disk output based on squashfs output
// - squashfs output's post scripts sets up some initial filesystem structure
// - disk output unpacks squashfs into the disk image, with the mountpoints in place
// - disk output now has a new set of post scripts that do some additional setup, like bootloader setup
// and such
// 
// - or an ISO output that copies the squash image itself to the ISO and sets up the bootloader
// to load that squash image as root


output "iso" "xfce" {
    // There will be no bootstrapping, we're just repacking an existing output
    bootstrap_method = "none"
    
    // We do not mount ISO files like a rootfs, so we will be using the custom URI paths scheme
    
    partition_layout {
        partition {
            // partno is used, label may not be included in the resulting ISO
            // We can use the label to refer to the partition in the copy_files directive
            label = "esp"
            type = "esp"
            filesystem = "fat32"
        }
        partition {
            label = "content"
            type = "isofs"
        }
    }
    copy {
        // source = output.squashfs.xfce.output_file // evaluated from fs.katsu.hcl
        destination = "content:/LiveOS/xfce.iso" // somehow evaluate from context of current scope from partition_layout?
    }
}