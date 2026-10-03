use crate::{
	backends::{bootloader::Bootloader, fs_tree::RootBuilder},
	bail_let,
	cli::OutputFormat,
	config::{Manifest, Script},
	feature_flag_bool, feature_flag_str,
	rootimg::erofs::{CompressHints, MkfsErofsOptions, erofs_mkfs},
	util::{just_write, loopdev_with_file},
};
use color_eyre::{Result, eyre::bail};
use indexmap::IndexMap;
use std::{
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

/// Which live-media integration the initramfs should carry.
///
/// The three layouts boot through different mechanisms, so the kernel arguments
/// and units appended to the initramfs differ even though the media payload is
/// the same shape in all cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveLayout {
	/// A plain rootfs: no deployment selection, no katsu integration.
	Plain,
	/// An OSTree sysroot, selected by `ostree=` and assembled by
	/// `ostree-prepare-root` with our own overrides.
	Ostree,
	/// A native composefs root, selected by `composefs=<D>` and assembled by
	/// bootc's `bootc-root-setup.service`.
	Composefs,
}

pub const WORKDIR: &str = "katsu-work";
pub const BOOTIMGS: &str = "boot_imgs";

pub fn default_true() -> bool {
	true
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
		warn!(
			"Script does not have shebang, #!/bin/sh will be added. It is recommended to add a shebang to your script."
		);
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
	"livenet dmsquash-live dmsquash-live-autooverlay convertfs pollcdrom qemu qemu-net systemd";
const DR_OMIT: &str = "";
const DR_ARGS: &str = "-vv --xz --reproducible";

impl IsoBuilder {
	/// Switch fragment deduplication to the content-comparing `full` mode.
	///
	/// `mkfs.erofs` accepts only `inode` and `full`. `inode` dedupes solely when
	/// inode data is identical; `full` compares every fragment's content, which is
	/// more thorough but much heavier and has OOM-killed this host on a large tree.
	fn set_fragdedupe_full(extra_features: &mut [String]) {
		for feature in extra_features.iter_mut() {
			if feature == "fragdedupe=inode" {
				*feature = "fragdedupe=full".to_string();
			}
		}
	}

	/// Produce the ISO initramfs for a composefs layout.
	///
	/// A composefs tree has no `usr/` at its root, so dracut cannot read it
	/// directly; the OS lives inside the composefs image. That image *does* contain
	/// a full tree (`usr/lib/modules/<kver>` and everything dracut needs), so mount
	/// it and generate against the mounted image instead of the tree root. This
	/// yields an initramfs built specifically for the live media rather than the
	/// image's disk-boot one, which would still carry its own root selection.
	fn composefs_dracut(&self, root: &Path, workspace: &Path) -> Result<PathBuf> {
		// A composefs image is metadata-only and must be resolved against its object
		// store. Mounting it through the `composefs` library (the same one bootc uses)
		// is required: a bare EROFS mount exposes directory entries but not file
		// contents, because the image references objects under `composefs/objects/`.
		//
		// The repository must be read from the *staging* mount, not the ISO tree: the
		// tree is an overlayfs, and the kernel refuses to mount an EROFS image whose
		// backing file lives on overlayfs (`ENOTBLK`). Both are views of the same
		// data; only the plain mount works here.
		let staging_repo = workspace.join("unified-root/composefs");
		let repo = if staging_repo.is_dir() { staging_repo } else { root.join("composefs") };
		let deployment = crate::backends::bootloader::ComposefsDeployment::resolve(root)?
			.ok_or_else(|| {
				color_eyre::eyre::eyre!("No composefs BLS entry under {}", root.display())
			})?;
		let mountpoint = workspace.join("composefs-root");
		deployment.mount(&repo, &mountpoint)?;
		// The mount must outlive dracut, so release it after generating.
		let guard = crate::backends::fs_tree::StagedRoot::new(mountpoint.clone());
		let result = self.dracut_generate(&mountpoint, workspace, LiveLayout::Composefs);
		drop(guard);
		result
	}

	fn dracut(&self, root: &Path, workspace: &Path, live: LiveLayout) -> Result<PathBuf> {
		match live {
			// A composefs tree has no `usr/` at its root, so the OS tree is taken
			// from the mounted composefs image instead.
			LiveLayout::Composefs => self.composefs_dracut(root, workspace),
			_ => self.dracut_generate(root, workspace, live),
		}
	}

	fn live_root_image(image_dir: &Path, ostree_sysroot: bool) -> Result<PathBuf> {
		let squash_image = image_dir.join("squashfs.img");
		if !ostree_sysroot {
			return Ok(squash_image);
		}

		// dmsquash-live only accepts /usr or nested LiveOS images inside
		// squashfs.img. Its direct rootfs.img path also accepts OSTree sysroots.
		let root_image = image_dir.join("rootfs.img");
		if squash_image.exists() {
			// Reuse images from earlier builds when rootimg is skipped, without
			// retaining a second payload that dracut would select first.
			if root_image.exists() {
				fs::remove_file(&squash_image)?;
			} else {
				fs::rename(&squash_image, &root_image)?;
			}
		}
		Ok(root_image)
	}

	/// Run dracut against `source_root`, writing the live initramfs into the ISO tree.
	fn dracut_generate(&self, root: &Path, workspace: &Path, live: LiveLayout) -> Result<PathBuf> {
		bail_let!(
			Some(kver) = fs::read_dir(root.join("usr/lib/modules"))?.find_map(|f| {
				trace!(?f, "File in /usr/lib/modules");
				f.ok()
					.and_then(|entry| entry.file_name().to_str().map(|s| s.to_string()))
			}) => "Can't find any kernel version in /usr/lib/modules"
		);
		info!(?kver, "Found kernel version");
		info!(?root, "Generating initramfs");

		// set dracut options
		// this is kind of a hack, but uhh it works maybe

		let default_modules = match live {
			LiveLayout::Plain => DR_MODS,
			// The OSTree path selects the deployment via `ostree=`, and drives it
			// from our own prepare-root overrides.
			LiveLayout::Ostree => "ostree systemd qemu qemu-net",
			// Composefs root assembly is bootc's job, so the `bootc` module (and its
			// `bootc-root-setup.service`) is required rather than omitted.
			LiveLayout::Composefs => "bootc systemd qemu qemu-net",
		};
		let dr_mods = feature_flag_str!("dracut-mods").unwrap_or(default_modules.to_string());
		let mut dr_omit = feature_flag_str!("dracut-omit").unwrap_or(DR_OMIT.to_string());
		match live {
			LiveLayout::Plain => {},
			LiveLayout::Ostree => {
				// Do not let generic live or composefs generators race our sysroot mount.
				dr_omit.push_str(" dmsquash-live dmsquash-live-autooverlay livenet bootc");
			},
			LiveLayout::Composefs => {
				// The generic live generators would race our own sysroot.mount.
				//
				// `ostree` is not omitted: the image's own dracut configs
				// (`20-bootc-base.conf` and the Ostree generator's own `55-ostree.conf`)
				// add it, so omitting it there is what breaks the build. Its
				// `installkernel` is the only thing that pulls `erofs` and `overlay` into
				// the initramfs, and without them dracut aborts with a hard error:
				// "Module 'overlayfs' will not be installed, because kernel module
				// 'overlay' is not available" / "Module 'iscsi' cannot be installed".
				// The `50bootc` module installs those same two modules *unconditionally*
				// rather than gated on the dependencies check, so with it carried over
				// from the image's initramfs the boot path is unaffected either way.
				dr_omit.push_str(" dmsquash-live dmsquash-live-autooverlay livenet");
			},
		}

		let binding = feature_flag_str!("dracut-args").unwrap_or(DR_ARGS.to_string());
		let mut dr_args = shellish_parse::parse(&binding, false)
			.map_err(|e| color_eyre::eyre::eyre!("Invalid dracut arguments: {e}"))?;
		// Empty positional arguments make dracut select its default output path.
		dr_args.retain(|arg| !arg.is_empty());
		dr_args.extend([
			"--nomdadmconf".to_string(),
			"--nolvmconf".to_string(),
			"-fN".to_string(),
			"-a".to_string(),
			dr_mods,
		]);
		if !dr_omit.is_empty() {
			dr_args.push("--omit".to_string());
			dr_args.push(dr_omit);
		}
		// dracut wants a writable scratch directory and defaults to `$sysroot/var/tmp`,
		// which cannot work against a read-only composefs mount. Point it at the
		// workspace instead of making the image writable, which its whole design
		// avoids.
		if matches!(live, LiveLayout::Composefs) {
			let tmpdir = workspace.join("dracut-tmp");
			std::fs::create_dir_all(&tmpdir)?;
			dr_args.push("--tmpdir".to_string());
			dr_args.push(tmpdir.display().to_string());
		}
		let mut cmd = std::process::Command::new("dracut");

		// bootc/ostree deployments have no /boot of their own: ostree keeps the
		// kernel at the sysroot level. Chrooting into the deployment and letting
		// dracut write to /boot therefore produces nothing, so generate outside the
		// chroot and write straight to the ISO tree instead.
		let ostree_layout = root.join("usr/lib/ostree-boot").exists()
			|| root.join("usr/lib/ostree/prepare-root.conf").exists();
		let dracut_outside_chroot = feature_flag_bool!("dracut-outside-chroot") || ostree_layout;
		if ostree_layout && !feature_flag_bool!("dracut-inside-chroot") {
			info!("OSTree deployment detected, generating initramfs outside the chroot");
		}

		if dracut_outside_chroot {
			cmd.arg("-r");
			cmd.arg(root.canonicalize()?);
		}

		// The composefs layout reaches the OS tree through a mount that can be released
		// by the kernel once `katsu` stops holding it, which leaves dracut running with
		// a `-r` that no longer resolves. That makes module probes fail and dracut exit
		// with `Module 'iscsi' cannot be installed.`, so a removable backing store is a
		// hard error rather than something to paper over.
		if dracut_outside_chroot && !root.is_dir() {
			bail!(
				"composefs root {} is not readable when generating the initramfs",
				root.display()
			);
		}

		if !matches!(live, LiveLayout::Plain) {
			cmd.arg("--install").arg("mount mkdir realpath touch ln systemd-escape checkisomd5");
			cmd.arg("--add-drivers").arg("erofs squashfs overlay loop iso9660");
			cmd.arg("--no-hostonly-cmdline");
		}
		// `-r` rebases *units and files* onto the sysroot, but dracut still reads
		// kernel modules from the build host's `/lib/modules`. On a machine whose
		// kernel matches the image's that silently works; anywhere else (CI) every
		// module probe fails, and an optional module whose `check()` returns 1 —
		// `iscsi` is the usual one — aborts the build with
		// `Module 'iscsi' cannot be installed.` Point dracut at the image's own
		// modules instead, which for a composefs layout live inside the mounted
		// image rather than in the tree.
		let kmoddir = root.join("usr/lib/modules").join(&kver);
		if kmoddir.is_dir() {
			// Resolve so dracut does not interpret the path relative to whatever
			// working directory it happens to change into.
			let kmoddir = kmoddir.canonicalize()?;
			debug!(?kmoddir, "Using kernel modules from the source root");
			cmd.arg("--kmoddir").arg(&kmoddir);
		}
		let cmd = cmd.args(&dr_args).arg("--kver").arg(&kver);

		let current_dir = std::env::current_dir()?;
		info!(?current_dir, "Current directory");
		// https://github.com/dracut-ng/dracut-ng/issues/443
		// fixes a weird quirk in bootc builds, may need a check for bootc though since it kinda breaks SELinux
		let is_bootc_image = feature_flag_bool!("dracut-bootc")
			|| root.join("usr/lib/ostree-boot").exists()
			|| root.join("usr/lib/ostree/prepare-root.conf").exists();
		if is_bootc_image {
			info!("Detected bootc image, setting DRACUT_NO_XATTR=1");
			cmd.env("DRACUT_NO_XATTR", "1");
		}
		info!(?cmd, "Running dracut command");

		// Prepare iso-tree path for later
		let iso_tree_path = workspace.join(ISO_TREE);
		std::fs::create_dir_all(iso_tree_path.join("boot"))?;
		let final_initramfs_path = iso_tree_path.join("boot").join("initramfs.img");
		let pending_initramfs_path = iso_tree_path.join("boot").join("initramfs.img.pending");

		if dracut_outside_chroot {
			info!("Dracut run outside chroot, generating to iso-tree");
			cmd.arg(&pending_initramfs_path);
			let status = cmd.status()?;
			debug!(?status, "Dracut command finished");
			if !status.success() {
				bail!("Dracut failed with exit code: {}", status);
			}
			if !pending_initramfs_path.is_file() {
				bail!("Dracut succeeded but did not write {}", pending_initramfs_path.display());
			}
			match live {
				LiveLayout::Ostree => {
					crate::initramfs::append_ostree_live(&pending_initramfs_path, workspace)?
				},
				LiveLayout::Composefs => {
					crate::initramfs::append_composefs_live(&pending_initramfs_path, workspace)?
				},
				LiveLayout::Plain => {},
			}
			fs::rename(&pending_initramfs_path, &final_initramfs_path)?;
		} else {
			// FIXME(dracut): @korewaChino #43 - dracut ignores CLI initramfs path and writes to /boot.
			// Workaround: allow dracut to write to /boot then move the initramfs into place.
			// Details: dracut appears to ignore the positional/flag argument for initramfs path;
			// tracked in https://github.com/FyraLabs/katsu/issues/43. Remove when upstream
			// fixes or we implement an alternative generation path.
			crate::util::enter_chroot_run(root, || -> Result<()> {
				cmd.arg(format!("/boot/initramfs-{}.img", kver));

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
	/// Map an `erofs-fragments` flag value to its `mkfs.erofs -E` token.
	///
	/// Both the token names and the shorter aliases are accepted. The tokens are
	/// what show up in the logged `mkfs.erofs` command line and in CI matrices, so
	/// requiring a different spelling here is a needless trap. `None` means the
	/// feature is left off entirely.
	fn fragments_token(mode: &str) -> Result<Option<&'static str>> {
		match mode {
			"none" => Ok(None),
			"fragments" | "plain" => Ok(Some("fragments")),
			"all-fragments" | "all" => Ok(Some("all-fragments")),
			other => bail!(
				"invalid erofs-fragments value {other:?}; expected `none`, `fragments` \
				 (alias `plain`) or `all-fragments` (alias `all`)"
			),
		}
	}

	/// Build an EROFS image from `root`.
	///
	#[allow(dead_code)]
	pub fn erofs(&self, root: &Path, image: &Path) -> Result<()> {
		self.erofs_with_selinux_root(root, root, image)
	}

	/// Wrap a built EROFS payload in the GPT disk image the live media ships.
	///
	/// The ESP comes from the bootc staging image rather than being synthesised, so
	/// the live system's bootloader state matches a real install. The `/boot` tree
	/// goes into an XBOOTLDR partition, mirroring the staging layout, and the EROFS
	/// replaces its Btrfs root.
	fn wrap_payload_in_gpt(
		staging_image: &Path, payload: &Path, tree_root: &Path, destination: &Path,
		workspace: &Path,
	) -> Result<()> {
		use crate::rootimg::gpt::{WrapOptions, find_staging_esp, wrap_in_gpt};

		// The staging image is loop-attached by the `root` phase's output; read its
		// ESP from the whole-disk node so this works whether or not that is still
		// mounted.
		let (disk, handle) = crate::util::loopdev_with_file_and_parts(staging_image)?;
		let esp = find_staging_esp(&disk)?;
		let result = wrap_in_gpt(&WrapOptions {
			root_image: payload,
			destination,
			boot_tree: &tree_root.join("boot"),
			esp: &esp,
			scratch: &workspace.join("gpt-scratch"),
		});
		drop(handle);
		// The standalone EROFS is an intermediate; the GPT image now holds it.
		if result.is_ok() {
			fs::remove_file(payload).ok();
		}
		result
	}

	/// Like [`Self::erofs`], but reads SELinux contexts from `selinux_root`.
	///
	/// For an OSTree sysroot the contexts live under
	/// `ostree/deploy/<stateroot>/deploy/<csum>.0/{etc,usr/etc}/...`, not at the
	/// squash root, so passing the sysroot here would silently skip labelling and
	/// produce an image whose files have no SELinux labels.
	pub fn erofs_with_selinux_root(
		&self, root: &Path, selinux_root: &Path, image: &Path,
	) -> Result<()> {
		let mut opts = MkfsErofsOptions::default();

		// `mkfs.erofs --file-contexts` wants a single file. Newer images keep the
		// vendor copy under usr/etc; older ones only have /etc. Prefer the
		// machine-local /etc when present, since that is what the running system
		// would consult.
		let candidates = [
			"etc/selinux/targeted/contexts/files/file_contexts",
			"usr/etc/selinux/targeted/contexts/files/file_contexts",
		];
		let selinux_fcontexts =
			candidates.iter().map(|rel| selinux_root.join(rel)).find(|path| path.exists());

		match selinux_fcontexts {
			Some(path) => {
				debug!(?path, "Using SELinux file contexts for EROFS");
				opts.file_contexts = Some(path.display().to_string());
			},
			None => warn!(
				?selinux_root,
				"SELinux file contexts not found, skipping; the resulting image will \
				 have no SELinux labels"
			),
		}

		// The options below mirror `mkfs.erofs` so a build can be tuned without
		// editing code. Unset flags leave the measured defaults from
		// `MkfsErofsOptions::default` in place.
		if let Some(compression) = feature_flag_str!("erofs-compression") {
			info!(%compression, "Using configured EROFS compression");
			opts.compression = Some(compression);
		}
		// The level is a separate flag because the flag list is comma-separated and
		// a `zstd,level=6` value would be split apart, silently dropping the level.
		if let Some(level) = feature_flag_str!("erofs-compression-level") {
			if let Some(compression) = opts.compression.as_mut() {
				info!(%level, "Using configured EROFS compression level");
				*compression = format!("{compression},level={level}");
			} else {
				warn!("Ignoring erofs-compression-level without an erofs-compression algorithm");
			}
		}
		if let Some(chunk) = feature_flag_str!("erofs-chunk-size") {
			match chunk.parse::<u32>() {
				Ok(n) => {
					info!(chunk_size = n, "Using configured EROFS chunk size");
					opts.chunk_size = Some(n);
				},
				Err(_) => warn!(%chunk, "Ignoring non-numeric erofs-chunk-size value"),
			}
		}
		if let Some(xattr) = feature_flag_str!("erofs-xattr-level") {
			match xattr.parse::<u32>() {
				Ok(n) => {
					info!(xattr_level = n, "Using configured EROFS xattr level");
					opts.xattr_level = Some(n);
				},
				Err(_) => warn!(%xattr, "Ignoring non-numeric erofs-xattr-level value"),
			}
		}

		if let Some(workers) = feature_flag_str!("erofs-workers") {
			match workers.parse::<u32>() {
				Ok(n) => {
					info!(workers = n, "Using configured EROFS worker count");
					opts.workers = Some(n);
				},
				Err(_) => warn!(%workers, "Ignoring non-numeric erofs-workers value"),
			}
		}

		// A hints file lets hot, randomly-read paths use smaller physical clusters
		// without giving up large clusters for bulk data. The `live` strategy is the
		// default; a path uses that file instead, and `none` disables hints entirely.
		if let Some(hints) = feature_flag_str!("erofs-compress-hints") {
			opts.compress_hints = match hints.as_str() {
				"live" => Some(CompressHints::Live),
				"none" => None,
				other => Some(CompressHints::Path(PathBuf::from(other))),
			};
			info!(strategy = %hints, "Using EROFS compression hints");
		}

		// Global dedup is opt-in. It is single-threaded and index-based, so on a
		// large tree it costs minutes and gigabytes of RAM while `fragdedupe=inode`
		// already captures the duplicated files. See `MkfsErofsOptions::default`
		// for the measurement.
		if feature_flag_bool!("erofs-dedupe") {
			info!("Enabling EROFS global deduplication");
			opts.set_dedupe(true);
		}

		// Fragment dedup defaults to the measured-safe `inode` mode. `full` compares
		// every fragment's content and is much heavier; allow opting in when size
		// matters more than build stability.
		match feature_flag_str!("erofs-fragdedupe").as_deref() {
			Some("inode") | None => {},
			Some("full") => {
				info!("Using content-comparing fragment deduplication (slower, heavier)");
				Self::set_fragdedupe_full(&mut opts.extra_features);
			},
			Some(other) => {
				bail!("invalid erofs-fragdedupe value {other:?}; expected `inode` or `full`")
			},
		}
		// `fragdedupe` only applies with a fragments mode, so reject the combination
		// mkfs.erofs would otherwise silently ignore or reject itself.
		if let Some(mode) = feature_flag_str!("erofs-fragments") {
			let token = Self::fragments_token(&mode)?;
			opts.extra_features.retain(|f| !f.contains("fragments"));
			if let Some(token) = token {
				opts.extra_features.push(token.to_string());
			}
			info!(mode, "Using configured EROFS fragments mode");
		}

		erofs_mkfs(root, image, &opts)?;

		Ok(())
	}
	// TODO: add mac support
	pub fn xorriso(&self, image: &Path, manifest: &Manifest, workspace: &Path) -> Result<()> {
		info!("Generating ISO image");
		let volid = manifest.get_volid();
		let (uefi_bin, bios_bin) = self.bootloader.get_bins();
		let tree = workspace.join(ISO_TREE);
		let boot_imgs_dir = workspace.join(BOOTIMGS);

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
					// Hybrid BIOS boot needs the staged MBR. `cp_grub` already refused
					// to continue without it unless `no-grub-hybrid-mbr` was passed, so
					// its absence here means the opt-out was deliberate.
					"x86_64" if grub2_mbr_hybrid.is_file() => {
						vec!["--grub2-mbr", grub2_mbr_hybrid.to_str().unwrap()]
					},
					"x86_64" => {
						warn!("Building without a hybrid MBR: UEFI boot only");
						vec![]
					},
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
				debug!(
					"xorriso -as mkisofs --efi-boot {uefi_bin} -b {bios_bin} -no-emul-boot -boot-load-size 4 -boot-info-table --efi-boot {uefi_bin} -efi-boot-part --efi-boot-image --protective-msdos-label {root} -volid KATSU-LIVEOS -o {image}",
					root = tree.display(),
					image = image.display()
				);
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

		// A skipped `root` phase yields `None` rather than a result. Instead of
		// failing outright, fall back to a tree left by a previous build so later
		// phases (bootloader config, ISO assembly) can be iterated on cheaply.
		let tree_output = match phase!("root": self.root_builder.build(chroot, manifest)) {
			Some(output) => output,
			None => crate::backends::fs_tree::TreeOutput::from_existing(&workspace)?.ok_or_else(
				|| {
					color_eyre::eyre::eyre!(
						"'root' phase was skipped but no complete tree exists in {} to reuse",
						workspace.display()
					)
				},
			)?,
		};

		let tree_root = match &tree_output {
			crate::backends::fs_tree::TreeOutput::Tarball(_) => {
				bail!(
					"RootBuilder returned an image, but ISOBuilder requires a directory as rootfs - Unimplemented code path."
				);
			},
			// A sysroot ships wholesale (repo + deployment); the bootloader and
			// dracut phases operate on the deployment root inside it.
			_ => tree_output.rootfs()?,
		};
		let squash_root = tree_output.squash_root();
		debug!(?tree_root, ?squash_root, sysroot = tree_output.is_sysroot(), "Resolved tree roots");

		let _ = phase!("dracut": self.dracut(
			&tree_root,
			&workspace,
			match &tree_output {
				crate::backends::fs_tree::TreeOutput::UnifiedSysroot { .. } => LiveLayout::Composefs,
				crate::backends::fs_tree::TreeOutput::OstreeSysroot { .. } => LiveLayout::Ostree,
				_ => LiveLayout::Plain,
			}
		));

		// Clean up kernel artifacts from /boot before squashing
		// kernel-install will regenerate them on target system
		info!("Cleaning up kernel artifacts from chroot /boot before creating root image");
		let boot_dir = tree_root.join("boot");
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
		let root_image = Self::live_root_image(&image_dir, tree_output.is_sysroot())?;

		// For a plain rootfs this is the tree itself; for an OSTree sysroot it is
		// the whole sysroot, so that `/ostree/repo` and therefore the deployment
		// ship on the media and the system can boot without any runtime install.
		//
		// SELinux contexts are read from the deployment root (`tree_root`), which
		// for a sysroot is *inside* the squashed tree rather than at its top.
		//
		// The unified layout additionally wraps the payload in a GPT disk image:
		// bootc looks for an ESP among the backing devices of whatever it booted
		// from, and a bare EROFS loop provides none, so every bootc command —
		// including `bootc status` — would fail. Building the EROFS to a temporary
		// path leaves the final one free for the wrapper.
		let needs_gpt = tree_output.staging_image().is_some() && !feature_flag_bool!("no-erofs");
		let payload = if needs_gpt {
			let mut scratch = root_image.as_os_str().to_os_string();
			scratch.push(".erofs");
			PathBuf::from(scratch)
		} else {
			root_image.clone()
		};

		if feature_flag_bool!("no-erofs") {
			let _ = phase!("rootimg": self.squashfs(&squash_root, &payload));
		} else {
			let _ = phase!("rootimg": self.erofs_with_selinux_root(
				&squash_root,
				&tree_root,
				&payload
			));
		}

		if let Some(staging_image) = tree_output.staging_image().filter(|_| needs_gpt) {
			let _ = phase!("rootimg": Self::wrap_payload_in_gpt(
				staging_image,
				&payload,
				&tree_root,
				&root_image,
				&workspace
			));
		}

		let _ = phase!("copy-live": self.bootloader.copy_liveos(manifest, &tree_root, &workspace));
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

		let _ = phase!("iso": self.xorriso(&image, manifest, &workspace));

		let _ = phase!("bootloader": self.bootloader.install(&image));

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
	use super::*;

	#[test]
	fn ostree_live_image_reuses_existing_payload_without_duplication() {
		let dir = std::env::temp_dir().join(format!("katsu-live-image-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&dir).unwrap();
		fs::write(dir.join("squashfs.img"), b"erofs sysroot").unwrap();
		let image = IsoBuilder::live_root_image(&dir, true).unwrap();
		assert_eq!(image, dir.join("rootfs.img"));
		assert_eq!(fs::read(&image).unwrap(), b"erofs sysroot");
		assert!(!dir.join("squashfs.img").exists());

		fs::write(dir.join("squashfs.img"), b"stale image").unwrap();
		IsoBuilder::live_root_image(&dir, true).unwrap();
		assert!(!dir.join("squashfs.img").exists());
		assert_eq!(fs::read(&image).unwrap(), b"erofs sysroot");
		fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn plain_live_image_keeps_squashfs_name() {
		assert_eq!(
			IsoBuilder::live_root_image(Path::new("LiveOS"), false).unwrap(),
			Path::new("LiveOS/squashfs.img")
		);
	}

	#[test]
	fn boot_payload_live_marker_follows_the_karg() {
		use crate::backends::bootloader::BootPayload;

		// These strings are what the initramfs services gate on; picking the wrong
		// one activates the wrong handoff without any error.
		let ostree = BootPayload {
			karg: "ostree=/ostree/boot.1/um/checksum/0".into(),
			..Default::default()
		};
		assert_eq!(ostree.live_marker(), "rd.katsu.ostree");

		let composefs = BootPayload { karg: "composefs=abc123".into(), ..Default::default() };
		assert_eq!(composefs.live_marker(), "rd.katsu.composefs");

		// A plain rootfs has no katsu integration to activate.
		assert_eq!(BootPayload::default().live_marker(), "");
	}

	#[test]
	fn live_templates_emit_the_marker_matching_the_deployment() {
		// Each layout has its own initramfs service, gated on its own marker, so
		// emitting the wrong one silently activates the wrong handoff. Cover the
		// plain, OSTree and composefs cases explicitly.
		let cases = [
			("", "", false),
			("ostree=/ostree/boot.1/um/checksum/0", "rd.katsu.ostree", true),
			("composefs=abc123", "rd.katsu.composefs", true),
		];
		for template in [
			include_str!("../templates/grub.cfg.tera"),
			include_str!("../templates/limine.cfg.tera"),
			include_str!("../templates/refind.cfg.tera"),
		] {
			for (karg, marker, is_deployment) in cases {
				let mut context = tera::Context::new();
				for key in [
					"GRUB_PREPEND_COMMENT",
					"LIMINE_PREPEND_COMMENT",
					"REFIND_PREPEND_COMMENT",
					"volid",
					"distro",
					"vmlinuz",
					"initramfs",
					"cmd",
				] {
					context.insert(key, "test");
				}
				context.insert("ostree", karg);
				context.insert("marker", marker);
				let rendered = tera::Tera::one_off(template, &context, false).unwrap();
				let cmdlines: Vec<_> = rendered
					.lines()
					.filter(|line| line.contains("rd.live.image") || line.contains("rd.katsu."))
					.collect();
				assert!(!cmdlines.is_empty(), "no cmdline rendered for {karg:?}");
				for line in cmdlines {
					// A deployment layout gets its marker and no generic root=; a plain
					// rootfs gets the generic live path and no katsu marker at all.
					assert_eq!(line.contains(marker) && !marker.is_empty(), is_deployment);
					assert_eq!(line.contains("root=live:"), !is_deployment);
					assert_eq!(line.contains("rd.systemd.gpt_auto=0"), is_deployment);
					if is_deployment {
						assert!(line.contains(karg), "deployment karg missing from {line}");
					}
				}
			}
		}
	}

	#[test]
	fn erofs_options_map_to_mkfs_erofs_arguments() {
		use crate::rootimg::erofs::MkfsErofsOptions;

		// These are the switches the compression-test CI matrix drives, so assert the
		// values actually reach the command line rather than being dropped.
		let opts = MkfsErofsOptions {
			compression: Some("lzma,6".to_string()),
			chunk_size: Some(131072),
			..Default::default()
		};
		let args = opts.build_args();
		assert!(args.contains(&"-zlzma,6".to_string()));
		assert!(args.contains(&"-C131072".to_string()));
	}

	#[test]
	fn set_fragdedupe_full_is_a_noop_when_inode_is_absent() {
		// The rewrite must not invent the feature, since `fragdedupe` only applies
		// alongside a fragments mode.
		let mut features = vec!["all-fragments".to_string()];
		IsoBuilder::set_fragdedupe_full(&mut features);
		assert_eq!(features, vec!["all-fragments".to_string()]);
	}

	#[test]
	fn fragments_flag_accepts_both_token_and_alias_spellings() {
		// CI passes the mkfs.erofs `-E` token; the flag originally demanded a
		// different alias (`plain`), so the matrix failed with an "invalid value"
		// error that looked like a typo in the workflow. Both spellings must work.
		for mode in ["fragments", "plain"] {
			assert_eq!(IsoBuilder::fragments_token(mode).unwrap(), Some("fragments"), "{mode}");
		}
		for mode in ["all-fragments", "all"] {
			assert_eq!(IsoBuilder::fragments_token(mode).unwrap(), Some("all-fragments"), "{mode}");
		}
		assert_eq!(IsoBuilder::fragments_token("none").unwrap(), None);

		// Anything else must fail loudly rather than silently disabling fragments,
		// which would quietly change the shipped image.
		assert!(IsoBuilder::fragments_token("bogus").is_err());
	}

	#[test]
	fn fragdedupe_inode_is_the_default_and_full_is_opt_in() {
		use crate::rootimg::erofs::MkfsErofsOptions;
		// `inode` is the measured-safe setting: `full` compares every fragment and
		// has OOM-killed the build host on a large tree.
		let mut features = MkfsErofsOptions::default().extra_features;
		assert!(features.iter().any(|f| f == "fragdedupe=inode"));

		IsoBuilder::set_fragdedupe_full(&mut features);
		assert!(features.iter().any(|f| f == "fragdedupe=full"));
		assert!(!features.iter().any(|f| f == "fragdedupe=inode"));
		// The neighbouring features must survive the rewrite.
		assert!(features.iter().any(|f| f == "fragments"));
	}

	#[test]
	fn shellish_parse_empty() {
		assert!(shellish_parse::parse("", false).unwrap().is_empty());
	}
}
