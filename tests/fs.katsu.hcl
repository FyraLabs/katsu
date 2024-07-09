// squashfs outputs have no partition layout, we just throw everything and assume it's root
output "squashfs" "xfce" {
    bootstrap_method = "dnf"
    dnf {
        package_lists = [
            pkg_list.dnf.core,
            pkg_list.dnf.xfce
        ]
    }
    
    // optional copy_files directive to copy files to the filesystem, will be relative to root
    // May be redundant with partition.copy_files, but it's here for completeness
    copy {
        source = "./somefile"
        destination = "/somefile"
    }
}