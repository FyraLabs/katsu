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

pkg_list "dnf" "core" {
    default = [
        "@core",
        "kernel-*" // you can glob here because DNF lets you do that
    ]
    x86_64 = [
        "grub2-efi-x64",
        "grub2-efi-x64-modules"
    ]

    aarch64 = [
        "grub2-efi-aarch64",
        "grub2-efi-aarch64-modules"
    ]
    exclude = {
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

// target block
// a target is an artifact output

target "meow" {
    type = "idk"
    partition_layout {
        partition {
            label = "boot"
            mountpoint = "/boot"
            filesystem = "ext4"
        }
        partition {
            label = "root"
            mountpoint = "/"
            filesystem = "ext4"
        }
    }
}