use crate::{
	backends::bootloader::Bootloader,
	bail_let,
	cli::OutputFormat,
	config::{Manifest, Script},
	feature_flag_bool, feature_flag_str,
	rootimg::erofs::{erofs_mkfs, MkfsErofsOptions},
	util::{just_write, loopdev_with_file},
};
use color_eyre::{eyre::bail, Result};
use indexmap::IndexMap;
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

pub const WORKDIR: &str = "katsu-work";
pub const BOOTIMGS: &str = "boot_imgs";

const DNF_TRANS_COMMENT: &str = "Initial transaction from building with Katsu";

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
				podman build -t $deriv --network host --build-arg DERIVE_FROM=$image -f $derivation $context;
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

		// XXX: Wonder if we can use skopeo here instead of podman + tar
		let container_store = chroot.canonicalize()?.join("var/lib/containers/storage");
		let container_store_ovfs = container_store.join("overlay");
		std::fs::create_dir_all(&container_store)?;

		if self.embed_image {
			// redeclare container_store as string, so cmd_lib doesn't complain
			let container_store = container_store.display();
			let container_store_ovfs = container_store_ovfs.display();
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

		// commenting this out for now, chroot mounts shouldn't need to be remade when bootstrapping packages

		// span for DNF stuff
		{
			let chroot = chroot.clone();
			let dnf = dnf.clone();
			let packages = packages.clone();
			let options = options.clone();
			let releasever = releasever.clone();

			let mut cmd = std::process::Command::new(&dnf);
			cmd.arg("do")
				.arg("-y")
				.arg("--action=install")
				.arg(format!("--comment={DNF_TRANS_COMMENT}"))
				.arg("--setopt=tsflags=")
				.arg(format!("--releasever={}", releasever))
				.arg(format!("--installroot={}", chroot.display()))
				.args(&packages)
				.args(&options);
			info!(?cmd, "Running dnf command to install packages");

			let mut clean_cmd = std::process::Command::new(&dnf);
			clean_cmd.arg("clean").arg("all").arg(format!("--installroot={}", chroot.display()));
			crate::util::run_with_chroot(&chroot, || -> Result<()> {
				let status = cmd.status()?;
				if !status.success() {
					bail!("DNF command failed with status: {}", status);
				}
				info!(?clean_cmd, "Running dnf clean command");
				let clean_status = clean_cmd.spawn()?.wait()?;
				if !clean_status.success() {
					warn!("DNF clean command failed with status: {}", clean_status);
				}
				Ok(())
			})?;
		}

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
		&self, chroot: &Path, image: &Path, manifest: &Manifest, skip_phases: Vec<String>,
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
		&self, chroot: &Path, image: &Path, manifest: &Manifest, _: Vec<String>,
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
		&self, _chroot: &Path, _image: &Path, _manifest: &Manifest, _skip_phases: Vec<String>,
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
		&self, _chroot: &Path, _image: &Path, manifest: &Manifest, _skip_phases: Vec<String>,
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
	fn dracut(&self, root: &Path) -> Result<PathBuf> {
		bail_let!(
			Some(kver) = fs::read_dir(root.join("usr/lib/modules"))?.find_map(|f| {
				// find any directory
				trace!(?f, "File in /usr/lib/modules");
				f.ok()
				.and_then(|entry| entry.file_name().to_str().map(|s| s.to_string()))
			}) => "Can't find any kernel version in /usr/lib/modules"
		);
		info!(?kver, "Found kernel version");
		info!(?root, "Generating initramfs");

		// set dracut options
		// this is kind of a hack, but uhh it works maybe

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
		let mut cmd = std::process::Command::new("dracut");

		let dracut_outside_chroot = feature_flag_bool!("dracut-outside-chroot");

		if dracut_outside_chroot {
			cmd.arg("-r");
			cmd.arg(root.canonicalize()?);
		}

		let cmd = cmd.env("DRACUT_SYSTEMD", "0").args(&dr_args).arg("--kver").arg(&kver);

		let current_dir = std::env::current_dir()?;
		info!(?current_dir, "Current directory");
		info!(?cmd, "Running dracut command");

		// Prepare iso-tree path for later
		let iso_tree_path = root.join("../").join(ISO_TREE);
		std::fs::create_dir_all(iso_tree_path.join("boot"))?;
		let final_initramfs_path = iso_tree_path.join("boot").join("initramfs.img");

		if dracut_outside_chroot {
			info!("Dracut run outside chroot, generating to iso-tree");
			cmd.arg(&final_initramfs_path);
			let status = cmd.status()?;
			debug!(?status, "Dracut command finished");
			if !status.success() {
				bail!("Dracut failed with exit code: {}", status);
			}
		} else {
			// FIXME(dracut): @korewaChino #43 - dracut ignores CLI initramfs path and writes to /boot.
			// Workaround: allow dracut to write to /boot then move the initramfs into place.
			// Details: dracut appears to ignore the positional/flag argument for initramfs path;
			// tracked in https://github.com/FyraLabs/katsu/issues/43. Remove when upstream
			// fixes or we implement an alternative generation path.
			crate::util::enter_chroot_run(root, || -> Result<()> {
				cmd.arg(format!("/boot/initramfs-{}.img", &kver));

				let status = cmd.status()?;
				debug!(?status, "Dracut command finished");
				if !status.success() {
					bail!("Dracut failed with exit code: {}", status);
				}

				Ok(())
			})?;

			// Move from chroot/boot to iso-tree/boot
			let boot_initramfs = root.join(format!("boot/initramfs-{}.img", kver));
			if boot_initramfs.exists() {
				fs::copy(&boot_initramfs, &final_initramfs_path)?;
				info!("Copied initramfs from chroot /boot to iso-tree");
			} else {
				bail!("Dracut did not create expected initramfs at {}", boot_initramfs.display());
			}
		}

		Ok(final_initramfs_path)
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
			"xz" => "-comp xz",
			"zstd" => "-comp zstd -Xcompression-level 19",
			"lzma" => "-comp lzma",
			sqfs_comp => {
				warn!(?sqfs_comp, "unknown compression, passing directly to mksquashfs");
				sqfs_comp
			},
		};

		let extra_args = feature_flag_str!("squashfs-args").unwrap_or("".to_owned());

		info!("Squashing file system (mksquashfs)");
		std::process::Command::new("mksquashfs")
			.args([chroot, image])
			.args(shellish_parse::parse(sqfs_comp_args, false).unwrap())
			.args(["-b", "1048576", "-noappend", "-e", "/dev/", "-e", "/proc/", "-e", "/sys/"])
			.args(["-p", "/dev 755 0 0", "-p", "/proc 755 0 0", "-p", "/sys 755 0 0"])
			.args(shellish_parse::parse(&extra_args, false).unwrap())
			.status()?;

		Ok(())
	}
	#[allow(dead_code)]
	pub fn erofs(&self, chroot: &Path, image: &Path) -> Result<()> {
		let mut opts = MkfsErofsOptions::default();
		// selinux bs
		let selinux_fcontexts = chroot.join("etc/selinux/targeted/contexts/files/file_contexts");
		if selinux_fcontexts.exists() {
			opts.file_contexts = Some(selinux_fcontexts.display().to_string());
		} else {
			warn!("SELinux file contexts not found, skipping");
		}

		erofs_mkfs(chroot, image, &opts)?;

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
			Bootloader::REFInd => {
				std::process::Command::new("xorriso")
					.arg("-as")
					.arg("mkisofs")
					.arg("-iso-level")
					.arg("3")
					.arg("-full-iso9660-filenames")
					.arg("-joliet")
					.arg("-joliet-long")
					.arg("-rational-rock")
					.arg("-volid")
					.arg(volid)
					.arg("-eltorito-alt-boot")
					.arg("-e")
					.arg("boot/efiboot.img")
					.arg("-no-emul-boot")
					.arg("-append_partition")
					.arg("2")
					.arg("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
					.arg(&efiboot)
					.arg("-appended_part_as_gpt")
					.arg("-o")
					.arg(image)
					.arg(&tree)
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

pub const ISO_TREE: &str = "iso-tree";

impl ImageBuilder for IsoBuilder {
	fn build(
		&self, chroot: &Path, _: &Path, manifest: &Manifest, skip_phases: Vec<String>,
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

		// Clean up kernel artifacts from /boot before squashing
		// kernel-install will regenerate them on target system
		info!("Cleaning up kernel artifacts from chroot /boot before creating root image");
		let boot_dir = chroot.join("boot");
		if boot_dir.exists() {
			// Remove vmlinuz* and initramfs* files, but keep grub/, efi/, etc.
			if let Ok(entries) = fs::read_dir(&boot_dir) {
				for entry in entries.flatten() {
					let path = entry.path();
					let filename = entry.file_name();
					let name = filename.to_string_lossy();

					// Remove various kernel artifacts we don't need
					if name.contains("-rescue-")
					// hack: don't remove initramfs for now
					// || name.starts_with("initramfs")
					// || name.starts_with("initrd")
					// || name.starts_with("vmlinuz")
					// || name.starts_with("System.map")
					// || name.starts_with("config-")
					// || name.ends_with(".img") && !path.is_dir()
					{
						if let Err(err) = fs::remove_file(&path) {
							warn!(?err, ?path, "Failed to remove boot artifact");
						} else {
							debug!(?path, "Removed boot artifact");
						}
					}
				}
			}
		}

		// temporarily store content of iso
		let image_dir = workspace.join(ISO_TREE).join("LiveOS");
		fs::create_dir_all(&image_dir)?;

		if feature_flag_bool!("no-erofs") {
			phase!("rootimg": self.squashfs(chroot, &image_dir.join("squashfs.img")));
		} else {
			phase!("rootimg": self.erofs(chroot, &image_dir.join("squashfs.img")));
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
	pub skip_phases: Vec<String>,
}

impl KatsuBuilder {
	pub fn new(
		manifest: Manifest, output_format: OutputFormat, skip_phases: Vec<String>,
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

		self.image_builder.build(&chroot, &image, &self.manifest, self.skip_phases.clone())
	}
}

#[cfg(test)]
mod test {
	#[test]
	fn shellish_parse_empty() {
		assert!(shellish_parse::parse("", false).unwrap().is_empty());
	}
}
