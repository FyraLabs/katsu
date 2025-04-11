use crate::{
	bail_let,
	cli::{OutputFormat, SkipPhases},
	config::{Manifest, Script},
	feature_flag_bool, feature_flag_str,
	util::{just_write, loopdev_with_file},
};
use cmd_lib::{run_cmd, run_fun};
use color_eyre::{eyre::bail, Result};
use indexmap::IndexMap;
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

const WORKDIR: &str = "katsu-work";
const BOOTIMGS: &str = "boot_imgs";
crate::prepend_comment!(GRUB_PREPEND_COMMENT: "/boot/grub/grub.cfg", "Grub configurations", katsu::builder::Bootloader::cp_grub);
crate::prepend_comment!(LIMINE_PREPEND_COMMENT: "/boot/limine.cfg", "Limine configurations", katsu::builder::Bootloader::cp_limine);

/// Represents the bootloader types supported by Katsu
///
/// This enum defines the different bootloader implementations that can be used
/// when creating bootable images. Each variant corresponds to a specific
/// bootloader technology with its own installation and configuration methods.
#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Bootloader {
	#[default]
	/// Standard GRUB2 bootloader with UEFI support (default)
	Grub,
	/// GRUB2 bootloader configured for legacy BIOS systems
	GrubBios,
	/// Limine bootloader, a modern UEFI/BIOS bootloader
	Limine,
	/// systemd-boot, a simple UEFI boot manager
	SystemdBoot,
}

impl From<&str> for Bootloader {
	fn from(value: &str) -> Self {
		match &*value.to_lowercase() {
			"limine" => Self::Limine,
			"grub" | "grub2" => Self::Grub,
			"grub-bios" => Self::GrubBios,
			"systemd-boot" => Self::SystemdBoot,
			_ => {
				warn!("Unknown bootloader: {value}, falling back to GRUB");
				Self::Grub
			},
		}
	}
}

impl Bootloader {
	/// Installs the bootloader to the specified image
	///
	/// This method is responsible for actually installing the bootloader to the
	/// target image after it has been created. Different bootloaders require
	/// different installation procedures.
	///
	/// # Arguments
	///
	/// * `image` - The path to the image file where the bootloader will be installed
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn install(&self, image: &Path) -> Result<()> {
		match *self {
			Self::Grub => info!("GRUB is not required to be installed to image, skipping"),
			Self::Limine => cmd_lib::run_cmd!(limine bios-install $image 2>&1)?,
			Self::SystemdBoot => cmd_lib::run_cmd!(bootctl --image=$image install 2>&1)?,
			Self::GrubBios => {
				cmd_lib::run_cmd!(grub-install --target=i386-pc --boot-directory=$image/boot 2>&1)?
			},
		}
		Ok(())
	}
	/// Returns the paths to the UEFI and BIOS bootloader binaries
	///
	/// This method provides the relative paths to the bootloader binaries needed
	/// for creating bootable media. These paths are used during the ISO creation
	/// process to locate the appropriate files for UEFI and BIOS boot support.
	///
	/// # Returns
	///
	/// * A tuple of `(&'static str, &'static str)` containing:
	///   * First element: Path to the UEFI bootloader binary
	///   * Second element: Path to the BIOS bootloader binary
	pub fn get_bins(&self) -> (&'static str, &'static str) {
		match *self {
			Self::Grub => ("boot/efi/EFI/fedora/shim.efi", "boot/eltorito.img"),
			Self::Limine => ("boot/limine-uefi-cd.bin", "boot/limine-bios-cd.bin"),
			Self::GrubBios => todo!(),
			Self::SystemdBoot => todo!(),
		}
	}
	/// Copies vmlinuz and initramfs files from the chroot to a destination directory
	///
	/// This helper method locates the kernel (vmlinuz) and initial ramdisk (initramfs)
	/// files in the chroot environment and copies them to the destination directory.
	/// These are essential files for booting the system.
	///
	/// # Arguments
	///
	/// * `chroot` - The path to the chroot directory containing the kernel and initramfs
	/// * `dest` - The destination directory where the files should be copied
	///
	/// # Returns
	///
	/// * `Result<(String, String)>` - Success with file names or failure with error details
	///   * First element: The name of the kernel file (e.g., "vmlinuz")
	///   * Second element: The name of the initramfs file (e.g., "initramfs.img")
	fn cp_vmlinuz_initramfs(&self, chroot: &Path, dest: &Path) -> Result<(String, String)> {
		trace!("Finding vmlinuz and initramfs");

		// Prepare required directories
		std::fs::create_dir_all(dest.join("boot"))?;

		// Find kernel version and vmlinuz
		let (vmlinuz, kernel_version) = self.find_vmlinuz(chroot)?;
		debug!(?vmlinuz, ?kernel_version, "Kernel version and vmlinuz found");

		// Find initramfs
		let initramfs = self.find_initramfs(chroot)?;

		// Copy files to destination
		self.copy_boot_files(chroot, dest, &vmlinuz, &initramfs)?;

		Ok(("vmlinuz".to_string(), "initramfs.img".to_string()))
	}

	fn find_vmlinuz(&self, chroot: &Path) -> Result<(String, Option<String>)> {
		let modules_dir = chroot.join("usr/lib/modules");

		// Find kernel version from modules directory
		let mut kernels = fs::read_dir(&modules_dir)?;
		let kernel_version = kernels.find_map(|f| {
			trace!(?f, "File in /usr/lib/modules");
			f.ok().and_then(|entry| entry.file_name().to_str().map(|s| s.to_string()))
		});

		trace!("Kernel version found: {:?}", kernel_version);

		// Determine vmlinuz path based on kernel version
		let vmlinuz = if let Some(ref kernel_version) = kernel_version {
			modules_dir.join(kernel_version).join("vmlinuz").to_string_lossy().to_string()
		} else {
			// If no kernel version found, we'll try to find vmlinuz in boot directory later
			String::new()
		};

		Ok((vmlinuz, kernel_version))
	}

	fn find_initramfs(&self, chroot: &Path) -> Result<String> {
		let bootdir = chroot.join("boot");

		// Search for initramfs in boot directory
		for f in bootdir.read_dir()? {
			let f = f?;
			if !f.metadata()?.is_file() {
				continue;
			}

			let name = f.file_name();
			debug!(?name, "File in /boot");
			let name = name.to_string_lossy();

			// Skip rescue images
			if name.contains("-rescue-") {
				continue;
			}

			// Look for initramfs files
			if name == "initramfs.img" || name.starts_with("initramfs-") {
				return Ok(name.to_string());
			}
		}

		bail!("Cannot find initramfs in {:?}", bootdir)
	}

	fn copy_boot_files(
		&self, chroot: &Path, dest: &Path, vmlinuz: &str, initramfs: &str,
	) -> Result<()> {
		let bootdir = chroot.join("boot");

		trace!(vmlinuz, initramfs, "Copying vmlinuz and initramfs");

		// Copy vmlinuz to destination
		let vmlinuz_dest = dest.join("boot").join("vmlinuz");
		trace!(?vmlinuz, ?vmlinuz_dest, "Copying vmlinuz to destination");

		if let Err(e) = run_cmd!(cp -v $vmlinuz $vmlinuz_dest) {
			tracing::error!(?e, ?vmlinuz, ?vmlinuz_dest, "Failed to copy vmlinuz");
			return Err(e.into());
		}

		// Copy initramfs to destination
		if let Err(e) = run_cmd!(cp -v $bootdir/$initramfs $dest/boot/initramfs.img) {
			tracing::error!(?e, ?initramfs, "Failed to copy initramfs");
			return Err(e.into());
		}

		Ok(())
	}

	fn cp_limine(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		// complaint to rust: why can't you coerce automatically with umwrap_or()????
		info!("Copying Limine files");
		let distro = &manifest.distro.as_ref().map_or("Linux", |s| s);
		let cmd = &manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let root = chroot.parent().unwrap().join(ISO_TREE);
		// std::fs::create_dir_all(format!("./{distro}/LiveOS"))?;
		std::fs::create_dir_all(root.join("boot"))?;
		std::fs::copy(
			"/usr/share/limine/limine-uefi-cd.bin",
			root.join("boot/limine-uefi-cd.bin"),
		)?;
		std::fs::copy(
			"/usr/share/limine/limine-bios-cd.bin",
			root.join("boot/limine-bios-cd.bin"),
		)?;
		std::fs::copy("/usr/share/limine/limine-bios.sys", root.join("boot/limine-bios.sys"))?;

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &root)?;
		let volid = manifest.get_volid();

		// Generate limine.cfg
		let limine_cfg = root.join("boot/limine.cfg");
		crate::tpl!("limine.cfg.tera" => { LIMINE_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &limine_cfg);

		let binding = run_fun!(b2sum $limine_cfg)?;
		let liminecfg_b2h = binding.split_whitespace().next().unwrap();

		// enroll limine secure boot
		tracing::info_span!("Enrolling Limine Secure Boot").in_scope(|| -> Result<()> {
			Ok(run_cmd!(
				limine enroll-config $root/boot/limine-uefi-cd.bin $liminecfg_b2h 2>&1;
				limine enroll-config $root/boot/limine-bios.sys $liminecfg_b2h 2>&1;
			)?)
		})?;

		Ok(())
	}
	/// A clone of mkefiboot from lorax
	/// Currently only works for PC, no mac support
	fn mkefiboot(&self, chroot: &Path, _: &Manifest) -> Result<()> {
		let tree = chroot.parent().unwrap().join(ISO_TREE);

		// TODO: Add mac boot support

		// make EFI disk
		let sparse_path = &tree.join("boot/efiboot.img");
		crate::util::create_sparse(sparse_path, 25 * 1024 * 1024)?; // 15MiB

		// let's mount the disk as a loop device
		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		cmd_lib::run_cmd!(
			// Format disk with mkfs.fat
			mkfs.msdos $ldp -v -n EFI 2>&1;

			// Mount disk to /tmp/katsu.efiboot
			mkdir -p /tmp/katsu.efiboot;
			mount $ldp /tmp/katsu.efiboot;

			mkdir -p /tmp/katsu.efiboot/EFI/BOOT;
			cp -avr $tree/EFI/BOOT/. /tmp/katsu.efiboot/EFI/BOOT 2>&1;

			umount /tmp/katsu.efiboot;
		)?;

		drop(hdl);
		Ok(())
	}

	fn cp_grub(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		let iso_tree = chroot.parent().unwrap().join(ISO_TREE);
		let boot_imgs_dir = chroot.parent().unwrap().join(BOOTIMGS);
		// port from katsu 0.9.2 :3
		if self.get_arch_short(manifest) == "x86_64" {
			// Copy GRUB shit
			let hybrid_img = chroot.join("usr/lib/grub/i386-pc/boot_hybrid.img");
			std::fs::copy(&hybrid_img, boot_imgs_dir.join("boot_hybrid.img"))?;
		}

		// Create necessary directories
		self.create_grub_directories(&iso_tree, &boot_imgs_dir)?;

		// Prepare configuration variables
		let kernel_cmdline = manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let volid = manifest.get_volid();
		let distro = manifest.distro.as_ref().map_or("Linux", |s| s);

		// Copy kernel and initramfs
		let (vmlinuz, initramfs) =
			self.copy_kernel_and_initramfs(chroot, &boot_imgs_dir, &iso_tree)?;

		// Generate GRUB configuration
		self.generate_grub_config(&iso_tree, volid, distro, &vmlinuz, &initramfs, kernel_cmdline)?;

		// Set up EFI boot files
		self.setup_efi_boot_files(manifest, &iso_tree)?;

		// Generate GRUB images
		self.generate_grub_images(chroot, &iso_tree, manifest)?;

		// Create EFI boot image
		self.mkefiboot(chroot, manifest)?;

		Ok(())
	}

	fn create_grub_directories(&self, iso_tree: &Path, boot_imgs_dir: &Path) -> Result<()> {
		std::fs::create_dir_all(iso_tree)?;
		std::fs::create_dir_all(boot_imgs_dir)?;
		Ok(())
	}

	fn copy_kernel_and_initramfs(
		&self, chroot: &Path, boot_imgs_dir: &Path, iso_tree: &Path,
	) -> Result<(String, String)> {
		// Copy vmlinuz and initramfs to bootimgs directory
		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, boot_imgs_dir)?;

		// Clean existing boot directory if present
		let _ = std::fs::remove_dir_all(iso_tree.join("boot"));

		// Copy boot files from chroot to ISO tree
		cmd_lib::run_cmd!(cp -r $chroot/boot $iso_tree/)?;

		// Rename grub2 directory to grub if needed
		if iso_tree.join("boot/grub2").exists() && !iso_tree.join("boot/grub").exists() {
			std::fs::rename(iso_tree.join("boot/grub2"), iso_tree.join("boot/grub"))?;
		}

		// Copy vmlinuz and initramfs from bootimgs to ISO tree
		std::fs::copy(
			boot_imgs_dir.join("boot").join(&vmlinuz),
			iso_tree.join("boot").join(&vmlinuz),
		)?;

		std::fs::copy(
			boot_imgs_dir.join("boot").join(&initramfs),
			iso_tree.join("boot").join("initramfs.img"),
		)?;

		Ok((vmlinuz, "initramfs.img".to_string()))
	}

	fn generate_grub_config(
		&self, iso_tree: &Path, volid: String, distro: &str, vmlinuz: &str, initramfs: &str,
		kernel_cmdline: &str,
	) -> Result<()> {
		// Generate grub.cfg using template
		crate::tpl!(
			"grub.cfg.tera" => {
				GRUB_PREPEND_COMMENT,
				volid,
				distro,
				vmlinuz: vmlinuz.to_string(),
				initramfs: initramfs.to_string(),
				cmd: kernel_cmdline.to_string()
			} => iso_tree.join("boot/grub/grub.cfg")
		);

		Ok(())
	}

	fn setup_efi_boot_files(&self, manifest: &Manifest, iso_tree: &Path) -> Result<()> {
		// Determine architecture-specific values
		let arch_short = self.get_arch_short(manifest);
		let arch_short_upper = arch_short.to_uppercase();
		let arch_32 = self.get_arch_32bit(manifest).to_uppercase();

		// Create EFI directories
		std::fs::create_dir_all(iso_tree.join("EFI/BOOT/fonts"))?;

		// Copy and configure EFI files
		cmd_lib::run_cmd!(
			cp -av $iso_tree/boot/efi/EFI/fedora/. $iso_tree/EFI/BOOT;
			cp -av $iso_tree/boot/grub/grub.cfg $iso_tree/EFI/BOOT/BOOT.conf 2>&1;
			cp -av $iso_tree/boot/grub/grub.cfg $iso_tree/EFI/BOOT/grub.cfg 2>&1;
			cp -av $iso_tree/boot/grub/fonts/unicode.pf2 $iso_tree/EFI/BOOT/fonts;
			cp -av $iso_tree/EFI/BOOT/shim${arch_short}.efi $iso_tree/EFI/BOOT/BOOT${arch_short_upper}.efi;
			cp -av $iso_tree/EFI/BOOT/shim.efi $iso_tree/EFI/BOOT/BOOT${arch_32}.efi;
		)?;

		Ok(())
	}

	fn get_arch_short(&self, manifest: &Manifest) -> &'static str {
		match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH) {
			"x86_64" => "x64",
			"aarch64" => "aa64",
			_ => unimplemented!(),
		}
	}

	fn get_arch_32bit(&self, manifest: &Manifest) -> &'static str {
		match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH) {
			"x86_64" => "ia32",
			"aarch64" => "arm",
			_ => unimplemented!(),
		}
	}

	fn generate_grub_images(
		&self, chroot: &Path, iso_tree: &Path, manifest: &Manifest,
	) -> Result<()> {
		let host_arch = std::env::consts::ARCH;
		let target_arch = manifest.dnf.arch.as_deref().unwrap_or(host_arch);

		let arch = match target_arch {
			"x86_64" => "i386-pc",
			"aarch64" => "arm64-efi",
			_ => unimplemented!(),
		};

		let arch_out = match target_arch {
			"x86_64" => "i386-pc-eltorito",
			"aarch64" => "arm64-efi",
			_ => unimplemented!(),
		};

		let arch_modules = match target_arch {
			"x86_64" => vec!["biosdisk"],
			"aarch64" => vec!["efi_gop"],
			_ => unimplemented!(),
		};

		debug!("Generating Grub images");
		cmd_lib::run_cmd!(
			// Create eltorito.img for ISO boot
			grub2-mkimage -O $arch_out -d $chroot/usr/lib/grub/$arch -o $iso_tree/boot/eltorito.img -p /boot/grub iso9660 $[arch_modules] 2>&1;

			// Create rescue image for EFI files
			grub2-mkrescue -o $iso_tree/../efiboot.img;
		)?;

		debug!("Copying EFI files from Grub rescue image");
		let (loop_device, handle) = loopdev_with_file(&iso_tree.join("../efiboot.img"))?;

		cmd_lib::run_cmd!(
			mkdir -p /tmp/katsu-efiboot;
			mount $loop_device /tmp/katsu-efiboot;
			cp -r /tmp/katsu-efiboot/boot/grub $iso_tree/boot/;
			umount /tmp/katsu-efiboot;
		)?;

		drop(handle);

		Ok(())
	}

	/// Copies the bootloader files to the live OS image
	///
	/// This method copies all necessary bootloader files to the ISO tree to create
	/// a bootable live OS image. The specific files copied depend on the bootloader type.
	/// This is one of the main methods used during the ISO creation process.
	///
	/// # Arguments
	///
	/// * `manifest` - The manifest containing configuration information
	/// * `chroot` - The path to the chroot directory
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn copy_liveos(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		info!("Copying bootloader files");
		match *self {
			Self::Grub => self.cp_grub(manifest, chroot)?,
			Self::Limine => self.cp_limine(manifest, chroot)?,
			Self::SystemdBoot => todo!(),
			Self::GrubBios => self.cp_grub_bios(chroot)?,
		}
		Ok(())
	}

	/// Copies GRUB BIOS-specific files to the ISO tree
	///
	/// This method is responsible for setting up the legacy BIOS boot environment
	/// using GRUB. It's used when the bootloader type is GrubBios.
	///
	/// # Arguments
	///
	/// * `_chroot` - The path to the chroot directory
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn cp_grub_bios(&self, _chroot: &Path) -> Result<()> {
		todo!()
	}
}

pub trait RootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()>;
}

fn _default_dnf() -> String {
	String::from("dnf")
}

// credits to the Universal Blue people for figuring out how to build a bootc-based image :3
/// A bootc-based image. This is the second implementation of the RootBuilder trait.
/// This takes an OCI image and builds a rootfs out of it, optionally with a containerfile
/// to build a derivation specific to this image.
///
///
/// A derivation is a containerfile with 1 custom argument: `DERIVE_FROM`
///
/// It will be run as `podman build -t <image>:katsu-deriv --build-arg DERIVE_FROM=<image> -f <derivation> <CONTEXT>`
///
/// A containerfile should look like this:
///
/// ```dockerfile
/// ARG DERIVE_FROM
/// FROM $DERIVE_FROM
///
/// RUN echo "Hello from the containerfile!"
/// RUN touch /grass
///
/// # ... Do whatever you want here
/// ```
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct BootcRootBuilder {
	/// The original image to use as a base
	pub image: String,
	/// Path to a containerfile (Dockerfile) to build a derivation out of
	/// (Optional, if not specified, the image will be used as-is)
	pub derivation: Option<String>,
	pub context: Option<String>,

	#[serde(default = "default_true")]
	pub embed_image: bool,
}
fn default_true() -> bool {
	true
}
impl RootBuilder for BootcRootBuilder {
	fn build(&self, chroot: &Path, _manifest: &Manifest) -> Result<()> {
		let image = &self.image;

		// Pull the image for us
		info!("Loading OCI image");
		cmd_lib::run_cmd!(
			podman pull $image 2>&1;
		)?;
		info!("Current working directory: {}", std::env::current_dir()?.display());

		let context = self.context.as_deref().unwrap_or(".");

		// get pwd
		info!("Building OCI image");
		let d_image = if let Some(derivation) = &self.derivation {
			let og_image = image.split(':').next().unwrap_or(image);
			// get the image, but change the tag to katsu_<variant>
			let deriv = format!("{og_image}:katsu_deriv");

			cmd_lib::run_cmd!(
				podman build -t $deriv --build-arg DERIVE_FROM=$image -f $derivation $context;
			)?;
			deriv
		} else {
			image.to_string()
		};

		info!(?d_image, "Exporting OCI image");
		std::fs::create_dir_all(chroot)?;

		let container = cmd_lib::run_fun!(
			podman create --rm $d_image /bin/bash
		)?;

		cmd_lib::run_cmd!(
			podman export $container | sudo tar -xf - -C $chroot;
		)?;

		let container_store = chroot.canonicalize()?.join("var/lib/containers/storage");
		let container_store_ovfs = container_store.join("overlay");
		std::fs::create_dir_all(&container_store)?;

		if self.embed_image {
			info!(?chroot, ?image, "Copying OCI image to chroot's container store");

			// Push the original image to the chroot's container store, not the derived one
			cmd_lib::run_cmd!(
				podman push ${image} "containers-storage:[overlay@${container_store}]$image" --remove-signatures;
			)?;
			// Then we also unmount the thing so it doesn't get in the way
			// but we don't wanna fail entirely if this fails
			cmd_lib::run_cmd!(
				umount -f $container_store_ovfs 2>&1;
			)
			.ok();
		}

		Ok(())
	}
}

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct DnfRootBuilder {
	#[serde(default = "_default_dnf")]
	pub exec: String,
	#[serde(default)]
	pub packages: Vec<String>,
	#[serde(default)]
	pub options: Vec<String>,
	#[serde(default)]
	pub exclude: Vec<String>,
	#[serde(default)]
	pub releasever: String,
	#[serde(default)]
	pub arch: Option<String>,
	#[serde(default)]
	pub arch_packages: BTreeMap<String, Vec<String>>,
	#[serde(default)]
	pub arch_exclude: BTreeMap<String, Vec<String>>,
	#[serde(default)]
	pub repodir: Option<PathBuf>,
	#[serde(default)]
	pub global_options: Vec<String>,
}

impl RootBuilder for DnfRootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()> {
		info!("Running Pre-install scripts");

		run_all_scripts(&manifest.scripts.pre, chroot, false)?;

		// todo: generate different kind of fstab for iso and other builds
		if let Some(disk) = &manifest.disk {
			// write fstab to chroot
			crate::util::just_write(chroot.join("etc/fstab"), disk.fstab(chroot)?)?;
		}

		let mut packages = self.packages.clone();
		let mut options = self.options.clone();
		let mut exclude = self.exclude.clone();
		let releasever = &self.releasever;

		if let Some(a) = &self.arch {
			debug!(arch = ?a, "Setting arch");
			options.push(format!("--forcearch={a}"));
		}

		if let Some(reposdir) = &self.repodir {
			let reposdir = reposdir.canonicalize()?;
			let reposdir = reposdir.display();
			debug!(?reposdir, "Setting reposdir");
			options.push(format!("--setopt=reposdir={reposdir}"));
		}

		let chroot = chroot.canonicalize()?;

		// Get host architecture using uname
		let host_arch = std::env::consts::ARCH;

		let arch_string = self.arch.as_deref().unwrap_or(host_arch);

		if let Some(pkg) = self.arch_packages.get(arch_string) {
			packages.append(&mut pkg.clone());
		}

		if let Some(pkg) = self.arch_exclude.get(arch_string) {
			exclude.append(&mut pkg.clone());
		}

		let dnf = &self.exec;

		options.append(&mut exclude.iter().map(|p| format!("--exclude={p}")).collect());

		info!("Initializing system with dnf");
		crate::run_cmd_prep_chroot!(&chroot,
			$dnf install -y --releasever=$releasever --installroot=$chroot $[packages] $[options] 2>&1;
			$dnf clean all --installroot=$chroot;
		)?;

		info!("Setting up users");

		if manifest.users.is_empty() {
			warn!("No users specified, no users will be created!");
		} else {
			manifest.users.iter().try_for_each(|user| user.add_to_chroot(&chroot))?;
		}

		if manifest.bootloader == Bootloader::GrubBios || manifest.bootloader == Bootloader::Grub {
			info!("Attempting to run grub2-mkconfig");
			// crate::chroot_run_cmd!(&chroot,
			// 	echo "GRUB_DISABLE_OS_PROBER=true" > /etc/default/grub;
			// )?;

			// While grub2-mkconfig may not return 0 it should still work
			// todo: figure out why it still wouldn't write the file to /boot/grub2/grub.cfg
			//       but works when run inside a post script
			let res = crate::util::enter_chroot_run(&chroot, || {
				std::process::Command::new("grub2-mkconfig")
					.arg("-o")
					.arg("/boot/grub2/grub.cfg")
					.status()?;
				Ok(())
			});

			if let Err(e) = res {
				warn!(?e, "grub2-mkconfig not returning 0, continuing anyway");
			}

			// crate::chroot_run_cmd!(&chroot,
			// 	rm -f /etc/default/grub;
			// )?;
		}

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		run_all_scripts(&manifest.scripts.post, &chroot, true)
	}
}

#[tracing::instrument(skip(chroot, is_post))]
pub fn run_script(script: Script, chroot: &Path, is_post: bool) -> Result<()> {
	let id = script.id.as_ref().map_or("<NULL>", |s| s);
	bail_let!(Some(mut data) = script.load() => "Cannot load script `{id}`");
	let name = script.name.as_ref().map_or("<Untitled>", |s| s);

	info!(id, name, in_chroot = script.chroot, "Running script");

	let name = format!("script-{}", script.id.as_ref().map_or("untitled", |s| s));
	// check if data has shebang
	if !data.starts_with("#!") {
		warn!("Script does not have shebang, #!/bin/sh will be added. It is recommended to add a shebang to your script.");
		data.insert_str(0, "#!/bin/sh\n");
	}

	let mut tiffin = tiffin::Container::new(chroot.to_path_buf());

	if script.chroot.unwrap_or(is_post) {
		tiffin.run(|| -> Result<()> {
			// just_write(chroot.join("tmp").join(&name), data)?;
			just_write(PathBuf::from(format!("/tmp/{name}")), data)?;

			cmd_lib::run_cmd!(
				chmod +x /tmp/$name;
				/tmp/$name 2>&1;
				rm -f /tmp/$name;
			)?;

			Ok(())
		})??;
	} else {
		just_write(PathBuf::from(format!("katsu-work/{name}")), data)?;
		// export envar
		std::env::set_var("CHROOT", chroot);
		cmd_lib::run_cmd!(
			chmod +x katsu-work/$name;
			/usr/bin/env CHROOT=$chroot katsu-work/$name 2>&1;
			rm -f katsu-work/$name;
		)?;
	}

	info!(id, name, "Finished script");
	Ok(())
}

pub fn run_all_scripts(scrs: &[Script], chroot: &Path, is_post: bool) -> Result<()> {
	// name => (Script, is_executed)
	let mut scrs = scrs.to_owned();
	scrs.sort_by_cached_key(|s| s.priority);
	let scrs = scrs.iter().map(|s| (s.id.as_ref().map_or("<?>", |s| s), (s.clone(), false)));
	run_scripts(scrs.collect(), chroot, is_post)
}

#[tracing::instrument]
pub fn run_scripts(
	mut scripts: IndexMap<&str, (Script, bool)>, chroot: &Path, is_post: bool,
) -> Result<()> {
	trace!("Running scripts");
	for idx in scripts.clone().keys() {
		// FIXME: if someone dares to optimize things with unsafe, go for it
		// we can't use get_mut here because we need to do scripts.get_mut() later
		let Some((scr, done)) = scripts.get(idx) else { unreachable!() };
		if *done {
			trace!(idx, "Script is done, skipping");
			continue;
		}

		// Find needs
		let id = scr.id.clone().unwrap_or("<NULL>".into());
		let mut needs = IndexMap::new();
		let scr_needs_vec = &scr.needs.clone();
		for need in scr_needs_vec {
			// when funny rust doesn't know how to convert &String to &str
			bail_let!(Some((s, done)) = scripts.get_mut(need.as_str()) => "Script `{need}` required by `{id}` not found");

			if *done {
				trace!("Script `{need}` (required by `{idx}`) is done, skipping");
				continue;
			}
			needs.insert(need.as_str(), (std::mem::take(s), false));
			*done = true;
		}

		// Run needs
		run_scripts(needs, chroot, is_post)?;

		// Run the actual script
		let Some((scr, done)) = scripts.get_mut(idx) else { unreachable!() };
		run_script(std::mem::take(scr), chroot, is_post)?;
		*done = true;
	}
	Ok(())
}

pub trait ImageBuilder {
	fn build(
		&self, chroot: &Path, image: &Path, manifest: &Manifest, skip_phases: &SkipPhases,
	) -> Result<()>;
}
/// Creates a disk image, then installs to it
#[allow(dead_code)]
pub struct DiskImageBuilder {
	pub image: PathBuf,
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DiskImageBuilder {
	fn build(
		&self, chroot: &Path, image: &Path, manifest: &Manifest, _: &SkipPhases,
	) -> Result<()> {
		// create sparse file on disk
		bail_let!(Some(disk) = &manifest.disk => "Disk layout not specified");
		bail_let!(Some(disk_size) = &disk.size => "Disk size not specified");
		let sparse_path = &image.canonicalize()?.join("katsu.img");
		crate::util::create_sparse(sparse_path, disk_size.as_u64())?;

		// if let Some(disk) = manifest.disk.as_ref() {
		// 	disk.apply(&loopdev.path().unwrap())?;
		// 	disk.mount_to_chroot(&loopdev.path().unwrap(), &chroot)?;
		// 	disk.unmount_from_chroot(&loopdev.path().unwrap(), &chroot)?;
		// }
		let uefi = { self.bootloader != Bootloader::GrubBios };
		let arch = manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH);

		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		// Partition disk
		disk.apply(&ldp, arch)?;

		// Mount partitions to chroot
		disk.mount_to_chroot(&ldp, chroot)?;

		self.root_builder.build(&chroot.canonicalize()?, manifest)?;

		if !uefi {
			info!("Not UEFI, Setting up extra configs");

			// Let's use grub2-install to bless the disk

			info!("Blessing disk image with MBR");
			std::process::Command::new("grub2-install")
				.arg("--target=i386-pc")
				.arg(format!("--boot-directory={}", chroot.join("boot").display()))
				.arg(ldp)
				.output()
				.map_err(|e| color_eyre::eyre::eyre!("Failed to execute grub2-install: {}", e))?;
		}

		disk.unmount_from_chroot(chroot)?;

		drop(hdl);
		Ok(())
	}
}

/// Installs directly to a device
#[allow(dead_code)]
pub struct DeviceInstaller {
	pub device: PathBuf,
	pub bootloader: Bootloader,
	// root_builder
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DeviceInstaller {
	fn build(
		&self, _chroot: &Path, _image: &Path, _manifest: &Manifest, _skip_phases: &SkipPhases,
	) -> Result<()> {
		todo!();
		// self.root_builder.build(_chroot, _manifest)?;
		// Ok(())
	}
}

/// Installs as a raw chroot
#[allow(dead_code)]
pub struct FsBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for FsBuilder {
	fn build(
		&self, _chroot: &Path, _image: &Path, manifest: &Manifest, _skip_phases: &SkipPhases,
	) -> Result<()> {
		let out = manifest.out_file.as_ref().map_or("katsu-work/chroot", |s| s);
		let out = Path::new(out);
		// check if image exists, and is a folder
		if out.exists() && !out.is_dir() {
			bail!("Image path is not a directory");
		}

		// if image doesnt exist create it
		if !out.exists() {
			fs::create_dir_all(out)?;
		}

		self.root_builder.build(out, manifest)?;
		Ok(())
	}
}

pub struct IsoBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

const DR_MODS: &str =
	"livenet dmsquash-live dmsquash-live-autooverlay convertfs pollcdrom qemu qemu-net";
const DR_OMIT: &str = "";
const DR_ARGS: &str = "-vv --xz --reproducible";

impl IsoBuilder {
	fn dracut(&self, root: &Path) -> Result<String> {
		info!(?root, "Generating initramfs");
		bail_let!(
			Some(kver) = fs::read_dir(root.join("usr/lib/modules"))?.find_map(|f| {
				// find any directory
				trace!(?f, "File in /usr/lib/modules");
				f.ok()
					.and_then(|entry| entry.file_name().to_str().map(|s| s.to_string()))
			}) => "Can't find any kernel version in /usr/lib/modules"
		);

		info!(?kver, "Found kernel version");

		// set dracut options
		// this is kind of a hack, but uhh it works maybe
		// todo: make this properly configurable without envvars

		let dr_mods = feature_flag_str!("dracut-mods").unwrap_or(DR_MODS.to_string());
		let dr_omit = feature_flag_str!("dracut-omit").unwrap_or(DR_OMIT.to_string());

		let dr_extra_args = feature_flag_str!("dracut-args").unwrap_or("".to_string());
		let binding = feature_flag_str!("dracut-args").unwrap_or(DR_ARGS.to_string());
		let dr_basic_args = binding.split(' ').collect::<Vec<_>>();

		// combine them all into one string

		let dr_args2 = vec!["--nomdadmconf", "--nolvmconf", "-fN", "-a", &dr_mods, &dr_extra_args];
		let mut dr_args = vec![];

		dr_args.extend(dr_basic_args);

		dr_args.extend(dr_args2);
		if !dr_omit.is_empty() {
			dr_args.push("--omit");
			dr_args.push(&dr_omit);
		}

		//make dir
		std::fs::create_dir_all(root.join("boot"))?;

		crate::util::enter_chroot_run(root, || -> Result<()> {
			// get current dir
			let current_dir = std::env::current_dir()?;
			info!(?current_dir, "Current directory");
			std::process::Command::new("dracut")
				.env("DRACUT_SYSTEMD", "0")
				.args(&dr_args)
				.arg("--kver")
				.arg(&kver)
				.arg("/boot/initramfs.img")
				.status()?;
			Ok(())
		})?;

		Ok(root.join("boot").join("initramfs.img").to_string_lossy().to_string())
	}

	pub fn squashfs(&self, chroot: &Path, image: &Path) -> Result<()> {
		// Extra configurable options, for now we use envars
		// todo: document these

		let sqfs_comp = feature_flag_str!("squashfs-comp").unwrap_or("zstd".to_owned());

		info!("Determining squashfs options");

		let sqfs_comp_args = match sqfs_comp.as_str() {
			"gzip" => "-comp gzip -Xcompression-level 9",
			"lzo" => "-comp lzo",
			"lz4" => "-comp lz4 -Xhc",
			"xz" => "-comp xz -Xbcj x86",
			"zstd" => "-comp zstd -Xcompression-level 19",
			"lzma" => "-comp lzma",
			_ => bail!("Unknown squashfs compression: {sqfs_comp}"),
		}
		.split(' ')
		.collect::<Vec<_>>();

		let binding = feature_flag_str!("squashfs-args").unwrap_or("".to_owned());
		let sqfs_extra_args = binding.split(' ').collect::<Vec<_>>();

		info!("Squashing file system (mksquashfs)");
		std::process::Command::new("mksquashfs")
			.arg(chroot)
			.arg(image)
			.args(&sqfs_comp_args)
			.arg("-b")
			.arg("1048576")
			.arg("-noappend")
			.arg("-e")
			.arg("/dev/")
			.arg("-e")
			.arg("/proc/")
			.arg("-e")
			.arg("/sys/")
			.arg("-p")
			.arg("/dev 755 0 0")
			.arg("-p")
			.arg("/proc 755 0 0")
			.arg("-p")
			.arg("/sys 755 0 0")
			.args(&sqfs_extra_args)
			.status()?;

		Ok(())
	}
	#[allow(dead_code)]
	pub fn erofs(&self, chroot: &Path, image: &Path) -> Result<()> {
		std::process::Command::new("mkfs.erofs")
			.arg("-zlz4hc,level=12")
			.args(["--exclude-path", "/dev/"])
			.args(["--exclude-path", "/proc/"])
			.args(["--exclude-path", "/sys/"])
			.arg(image)
			.arg(chroot)
			.status()?;
		Ok(())
	}
	// TODO: add mac support
	pub fn xorriso(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		info!("Generating ISO image");
		let volid = manifest.get_volid();
		let (uefi_bin, bios_bin) = self.bootloader.get_bins();
		let tree = chroot.parent().unwrap().join(ISO_TREE);
		let boot_imgs_dir = chroot.parent().unwrap().join(BOOTIMGS);

		let grub2_mbr_hybrid = boot_imgs_dir.join("boot_hybrid.img");
		let efiboot = tree.join("boot/efiboot.img");

		match self.bootloader {
			Bootloader::Grub => {
				// cmd_lib::run_cmd!(grub2-mkrescue -o $image $tree -volid $volid 2>&1)?;
				// todo: normal xorriso command does not work for some reason, errors out with some GPT partition shenanigans
				// todo: maybe we need to replicate mkefiboot? (see lorax/efiboot)
				// however, while grub2-mkrescue works, it does not use shim, so we still need to manually call xorriso if we want to use shim
				// - @korewaChino, cc @madomado
				// It works, but we still need to make it use shim somehow
				// ok so, the partition layout should be like this:
				// 1. blank partition with 145,408 bytes
				// 2. EFI partition (fat12)
				// 3. data

				let arch_args = match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH)
				{
					// Hybrid mode is only supported on x86_64
					"x86_64" => vec!["--grub2-mbr", grub2_mbr_hybrid.to_str().unwrap()],
					"aarch64" => vec![],
					_ => unimplemented!(),
				};

				std::process::Command::new("xorrisofs")
					// Multi-extent ISO9660
					.args(["-iso-level", "3"])
					.arg("-R")
					.arg("-V")
					.arg(&volid)
					.args(&arch_args)
					.arg("-partition_offset")
					.arg("16")
					.arg("-appended_part_as_gpt")
					.arg("-append_partition")
					.arg("2")
					.arg("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
					.arg(&efiboot)
					.arg("-iso_mbr_part_type")
					.arg("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
					.arg("-c")
					.arg("boot.cat")
					.arg("--boot-catalog-hide")
					.arg("-b")
					.arg(bios_bin)
					.arg("-no-emul-boot")
					.arg("-boot-load-size")
					.arg("4")
					.arg("-boot-info-table")
					.arg("--grub2-boot-info")
					.arg("-eltorito-alt-boot")
					.arg("-e")
					.arg("--interval:appended_partition_2:all::")
					.arg("-no-emul-boot")
					.arg("-vvvvv")
					.arg("--md5")
					.arg(&tree)
					.arg("-o")
					.arg(image)
					.status()?;
			},
			_ => {
				debug!("xorriso -as mkisofs --efi-boot {uefi_bin} -b {bios_bin} -no-emul-boot -boot-load-size 4 -boot-info-table --efi-boot {uefi_bin} -efi-boot-part --efi-boot-image --protective-msdos-label {root} -volid KATSU-LIVEOS -o {image}", root = tree.display(), image = image.display());
				std::process::Command::new("xorriso")
					.args(["-iso-level", "3"])
					.arg("-as")
					.arg("mkisofs")
					.arg("-R")
					.arg("--efi-boot")
					.arg(uefi_bin)
					.arg("-b")
					.arg(bios_bin)
					.arg("-no-emul-boot")
					.arg("-boot-load-size")
					.arg("4")
					.arg("-boot-info-table")
					.arg("--efi-boot")
					.arg(uefi_bin)
					.arg("-efi-boot-part")
					.arg("--efi-boot-image")
					.arg("--protective-msdos-label")
					.arg(tree)
					.arg("-volid")
					.arg(volid)
					.arg("-o")
					.arg(image)
					.status()?;
			},
		}

		// implant MD5 checksums
		info!("Implanting MD5 checksums into ISO");
		std::process::Command::new("implantisomd5")
			.arg("--force")
			.arg("--supported-iso")
			.arg(image)
			.status()?;
		Ok(())
	}
}

const ISO_TREE: &str = "iso-tree";

impl ImageBuilder for IsoBuilder {
	fn build(
		&self, chroot: &Path, _: &Path, manifest: &Manifest, skip_phases: &SkipPhases,
	) -> Result<()> {
		crate::gen_phase!(skip_phases);
		// You can now skip phases by adding environment variable `KATSU_SKIP_PHASES` with a comma-separated list of phases to skip

		let image = PathBuf::from(manifest.out_file.as_ref().map_or("out.iso", |s| s));
		// Create workspace directory
		let workspace = chroot.parent().unwrap().to_path_buf();
		debug!("Workspace: {workspace:#?}");
		fs::create_dir_all(&workspace)?;

		phase!("root": self.root_builder.build(chroot, manifest));
		// self.root_builder.build(chroot.canonicalize()?.as_path(), manifest)?;

		phase!("dracut": self.dracut(chroot));

		// temporarily store content of iso
		let image_dir = workspace.join(ISO_TREE).join("LiveOS");
		fs::create_dir_all(&image_dir)?;

		if feature_flag_bool!("erofs") {
			phase!("rootimg": self.erofs(chroot, &image_dir.join("squashfs.img")));
		} else {
			phase!("rootimg": self.squashfs(chroot, &image_dir.join("squashfs.img")));
		}

		phase!("copy-live": self.bootloader.copy_liveos(manifest, chroot));
		// Reduce storage overhead by removing the original chroot
		// However, we'll keep an env flag to keep the chroot for debugging purposes
		if !feature_flag_bool!("keep-chroot")
			|| feature_flag_str!("keep-chroot").is_some_and(|s| s == "false")
		{
			info!("Removing chroot");
			// Try to unmount recursively first
			cmd_lib::run_cmd!(
				sudo umount -Rv $chroot;
			)
			.ok();
			fs::remove_dir_all(chroot)?;
		}

		phase!("iso": self.xorriso(chroot, &image, manifest));

		phase!("bootloader": self.bootloader.install(&image));

		Ok(())
	}
}

// todo: proper builder struct

pub struct KatsuBuilder {
	pub image_builder: Box<dyn ImageBuilder>,
	pub manifest: Manifest,
	pub skip_phases: SkipPhases,
}

impl KatsuBuilder {
	pub fn new(
		manifest: Manifest, output_format: OutputFormat, skip_phases: SkipPhases,
	) -> Result<Self> {
		let root_builder = match manifest.builder.as_ref().expect("Builder unspecified").as_str() {
			"dnf" => Box::new(manifest.dnf.clone()) as Box<dyn RootBuilder>,
			"bootc" => Box::new(manifest.bootc.clone()) as Box<dyn RootBuilder>,
			_ => todo!("builder not implemented"),
		};

		let bootloader = manifest.bootloader.clone();

		let image_builder = match output_format {
			OutputFormat::Iso => {
				Box::new(IsoBuilder { bootloader, root_builder }) as Box<dyn ImageBuilder>
			},
			OutputFormat::DiskImage => Box::new(DiskImageBuilder {
				bootloader,
				root_builder,
				image: PathBuf::from("./katsu-work/image/katsu.img"),
			}) as Box<dyn ImageBuilder>,
			OutputFormat::Folder => {
				Box::new(FsBuilder { bootloader, root_builder }) as Box<dyn ImageBuilder>
			},
			_ => todo!(),
		};

		Ok(Self { image_builder, manifest, skip_phases })
	}

	pub fn build(&self) -> Result<()> {
		let workdir = PathBuf::from(WORKDIR);

		let chroot = workdir.join("chroot");
		fs::create_dir_all(&chroot)?;

		let image = workdir.join("image");
		fs::create_dir_all(&image)?;

		self.image_builder.build(&chroot, &image, &self.manifest, &self.skip_phases)
	}
}
