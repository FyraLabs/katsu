use crate::chroot_run_cmd;
use bytesize::ByteSize;
use color_eyre::Result;
use merge_struct::merge;
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	io::Write,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace};

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Manifest {
	pub builder: String,
	#[serde(default)]
	pub import: Vec<PathBuf>,
	/// The distro name for the build result
	// entrypoint must have a distro name
	#[serde(default)]
	pub distro: Option<String>,

	/// Output file name
	// entrypoint must have an output location
	#[serde(default)]
	pub out_file: Option<String>,

	#[serde(default)]
	pub disk: Option<PartitionLayout>,

	/// DNF configuration
	// todo: dynamically load this?
	pub dnf: crate::builder::DnfRootBuilder,

	/// Scripts to run before and after the build
	#[serde(default)]
	pub scripts: ScriptsManifest,

	/// Users to add to the image
	#[serde(default)]
	pub users: Vec<Auth>,

	/// Extra parameters to the kernel command line in bootloader configs
	pub kernel_cmdline: Option<String>,
}

impl Manifest {
	/// Loads a single manifest from a file
	pub fn load(path: &Path) -> Result<Self> {
		let mut manifest: Self = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;

		// get dir of path relative to cwd

		let mut path_can = path.canonicalize()?;

		path_can.pop();
		trace!(path = ?path_can, "Canonicalizing path");

		for import in &mut manifest.import {
			debug!("Import: {import:#?}");
			*import = path_can.join(&import).canonicalize()?;
			debug!("Canonicalized import: {import:#?}");
		}

		// canonicalize all file paths in scripts, then modify their paths put in the manifest

		for script in &mut manifest.scripts.pre {
			if let Some(f) = script.file.as_mut() {
				trace!(?f, "Loading pre scripts");
				*f = path_can.join(&f).canonicalize()?;
			}
		}

		for script in &mut manifest.scripts.post {
			if let Some(f) = script.file.as_mut() {
				trace!(?f, "Loading post scripts");
				*f = path_can.join(&f).canonicalize()?;
			}
		}

		Ok(manifest)
	}

	/* 	pub fn list_all_imports(&self) -> Vec<PathBuf> {
		let mut imports = Vec::new();
		for import in self.import.clone() {
			let mut manifest = Self::load(import.clone()).unwrap();
			imports.append(&mut manifest.list_all_imports());
			imports.push(import);
		}
		imports
	} */

	pub fn load_all(path: &Path) -> Result<Self> {
		// get all imports, then merge them all
		let mut manifest = Self::load(path)?;

		// get dir of path

		let mut path_can = PathBuf::from(path);
		path_can.pop();

		for import in manifest.import.clone() {
			let imported_manifest = Self::load_all(&import)?;
			manifest = merge(&manifest, &imported_manifest)?;
		}

		Ok(manifest)
	}
}

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct ScriptsManifest {
	#[serde(default)]
	pub pre: Vec<Script>,
	#[serde(default)]
	pub post: Vec<Script>,
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq, Default)]
// load script from file, or inline if there's one specified
pub struct Script {
	pub id: Option<String>,
	pub name: Option<String>,
	pub file: Option<PathBuf>,
	pub inline: Option<String>,
	#[serde(default)]
	pub needs: Vec<String>,
}

impl Script {
	pub fn load(&self) -> Option<String> {
		if self.inline.is_some() {
			self.inline.clone()
		} else if let Some(f) = &self.file {
			std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok()
		} else {
			self.file
				.as_ref()
				.and_then(|f| std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok())
		}
	}
}

/// Utility function for determining partition /dev names
/// For cases where it's a mmcblk, or nvme, or loop device etc
pub fn partition_name(disk: &str, partition: usize) -> String {
	format!(
		"{disk}{}{partition}",
		if disk.starts_with("/dev/mmcblk")
			|| disk.starts_with("/dev/nvme")
			|| disk.starts_with("/dev/loop")
		{
			// mmcblk0p1 / nvme0n1p1 / loop0p1
			"p"
		} else {
			// sda1
			""
		}
	)
}

#[test]
fn test_dev_name() {
	let devname = partition_name("/dev/mmcblk0", 1);
	assert_eq!(devname, "/dev/mmcblk0p1");

	let devname = partition_name("/dev/nvme0n1", 1);
	assert_eq!(devname, "/dev/nvme0n1p1");

	let devname = partition_name("/dev/loop0", 1);
	assert_eq!(devname, "/dev/loop0p1");

	let devname = partition_name("/dev/sda", 1);
	assert_eq!(devname, "/dev/sda1");
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct PartitionLayout {
	pub size: Option<ByteSize>,
	pub partitions: Vec<Partition>,
}

impl PartitionLayout {
	pub fn new() -> Self {
		Self::default()
	}

	/// Adds a partition to the layout
	pub fn add_partition(&mut self, partition: Partition) {
		self.partitions.push(partition);
	}

	pub fn get_index(&self, mountpoint: &str) -> Option<usize> {
		// index should be +1 of the actual partition number (sda1 is index 0)

		self.partitions.iter().position(|p| p.mountpoint == mountpoint).map(|i| i + 1)
	}

	pub fn get_partition(&self, mountpoint: &str) -> Option<&Partition> {
		self.partitions.iter().find(|p| p.mountpoint == mountpoint)
	}

	pub fn sort_partitions(&self) -> Vec<(usize, Partition)> {
		// We should sort partitions by mountpoint, so that we can mount them in order
		// In this case, from the least nested to the most nested, so count the number of slashes

		// sort by least nested to most nested

		// However, also keep the original order of the partitions from the manifest

		// the key is the original index of the partition so we can get the right devname from its index

		let mut ordered = BTreeMap::new();

		for part in &self.partitions {
			let index = self.get_index(&part.mountpoint).unwrap();
			ordered.insert(index, part.clone());

			trace!(?index, ?part, "Index and partition");
		}

		// now sort by mountpoint, least nested to most nested by counting the number of slashes
		// but make an exception if it's just /, then it's 0

		// if it has the same number of slashes, sort by alphabetical order

		let mut ordered = ordered.into_iter().collect::<Vec<_>>();

		ordered.sort_unstable_by(|(_, a), (_, b)| {
			// trim trailing slashes

			let am = a.mountpoint.trim_end_matches('/').matches('/').count();
			let bm = b.mountpoint.trim_end_matches('/').matches('/').count();
			if a.mountpoint == "/" {
				// / should always come first
				std::cmp::Ordering::Less
			} else if b.mountpoint == "/" {
				// / should always come first
				std::cmp::Ordering::Greater
			} else if am == bm {
				// alphabetical order
				a.mountpoint.cmp(&b.mountpoint)
			} else {
				am.cmp(&bm)
			}
		});
		ordered
	}

	pub fn mount_to_chroot(&self, disk: &Path, chroot: &Path) -> Result<()> {
		// mount partitions to chroot

		// sort partitions by mountpoint
		let ordered: Vec<_> = self.sort_partitions();

		// Ok, so for some reason the partitions are swapped?
		for (index, part) in &ordered {
			let devname = partition_name(&disk.to_string_lossy(), *index);

			// clean the mountpoint so we don't have the slash at the start
			let mp_cleaned = part.mountpoint.trim_start_matches('/');
			let mountpoint = chroot.join(mp_cleaned);

			std::fs::create_dir_all(&mountpoint)?;

			trace!("mount {devname} {mountpoint:?}");

			cmd_lib::run_cmd!(mount $devname $mountpoint 2>&1)?;
		}

		Ok(())
	}

	pub fn unmount_from_chroot(&self, disk: &Path, chroot: &Path) -> Result<()> {
		// unmount partitions from chroot

		// sort partitions by mountpoint
		let ordered = self.sort_partitions().into_iter().rev().collect::<Vec<_>>();

		for (index, part) in &ordered {
			let devname = partition_name(&disk.to_string_lossy(), *index);

			// clean the mountpoint so we don't have the slash at the start
			let mp_cleaned = part.mountpoint.trim_start_matches('/');
			let mountpoint = chroot.join(mp_cleaned);

			std::fs::create_dir_all(&mountpoint)?;

			trace!("umount {devname} {mountpoint:?}");

			cmd_lib::run_cmd!(umount $devname 2>&1)?;
		}
		Ok(())
	}

	/// Generate fstab entries for the partitions
	pub fn fstab(&self, chroot: &Path) -> Result<String> {
		// sort partitions by mountpoint
		let ordered = self.sort_partitions();

		let mut fstab = String::new();

		const PREPEND_COMMENT: &str = r#"
# /etc/fstab: static file system information.
# Automatically generated by Katsu Image Builder. See 
# katsu::config::PartitionLayout::fstab for more information.
		
		"#;

		fstab.push_str(PREPEND_COMMENT.trim());

		const LEGEND: &str =
			"# <file system>\t<mount point>\t<type>\t<options>\t<dump>\t<pass>\n\n";

		fstab.push_str(LEGEND.trim());

		for part in ordered.iter().map(|(_, p)| p) {
			// get devname by finding from mount, instead of index because we won't be using it as much
			let mountpoint = PathBuf::from(&part.mountpoint);
			let mountpoint_chroot = part.mountpoint.trim_start_matches('/');
			let mountpoint_chroot = chroot.join(mountpoint_chroot);

			debug!(?mountpoint, "Mountpoint of partition");
			debug!(?mountpoint_chroot, "Mountpoint of partition in chroot");

			let devname = cmd_lib::run_fun!(findmnt -n -o SOURCE $mountpoint_chroot)?;

			debug!(?devname, "Device name of partition");

			// We will generate by UUID

			let uuid = cmd_lib::run_fun!(blkid -s UUID -o value $devname)?;

			debug!(?uuid, "UUID of partition");

			// clean the mountpoint so we don't have the slash at the start
			// let mp_cleaned = part.mountpoint.trim_start_matches('/');

			let fsname = if part.filesystem == "efi" { "vfat" } else { &part.filesystem };

			let fsck = if part.filesystem == "efi" { "0" } else { "2" };

			let entry = format!(
				"UUID={uuid}\t{mp}\t{fsname}\tdefaults\t0\t{fsck}",
				mp = mountpoint.to_string_lossy(),
			);

			fstab.push_str(&entry);
			fstab.push('\n');
		}

		Ok(fstab)
	}

	pub fn apply(&self, disk: &PathBuf) -> Result<()> {
		// This is a destructive operation, so we need to make sure we don't accidentally wipe the wrong disk

		info!("Applying partition layout to disk: {disk:#?}");

		// format disk with GPT

		trace!("Formatting disk with GPT");
		trace!("parted -s {disk:?} mklabel gpt");
		cmd_lib::run_cmd!(parted -s $disk mklabel gpt 2>&1)?;

		// create partitions

		let mut last_end = 0;

		for (i, part) in self.partitions.iter().enumerate() {
			trace!("Creating partition {i}: {part:#?}");

			// get index of partition
			let index = self.get_index(&part.mountpoint).unwrap();
			trace!("Index: {index}");

			let devname = partition_name(&disk.to_string_lossy(), index);

			let start_string = if i == 0 {
				// create partition at start of disk
				"1MiB".to_string()
			} else {
				// create partition after last partition
				ByteSize::b(last_end).to_string_as(true).replace(' ', "")
			};

			let end_string = if let Some(size) = part.size {
				// create partition with size
				last_end += size.as_u64();

				// remove space for partition table
				ByteSize::b(last_end).to_string_as(true).replace(' ', "")
			} else {
				// create partition at end of disk
				"100%".to_string()
			};

			let parted_fs = if part.filesystem == "efi" { "fat32" } else { "ext4" };

			trace!("parted -s {disk:?} mkpart primary {parted_fs} {start_string} {end_string}");

			cmd_lib::run_cmd!(parted -s $disk mkpart primary $parted_fs $start_string $end_string 2>&1)?;

			if part.filesystem == "efi" {
				trace!("parted -s {disk:?} set {index} esp on");
				cmd_lib::run_cmd!(parted -s $disk set $index esp on 2>&1)?;
			}

			if let Some(label) = &part.label {
				trace!("parted -s {disk:?} name {index} {label}");
				cmd_lib::run_cmd!(parted -s $disk name $index $label 2>&1)?;
			}

			// time to format the filesystem

			let fsname = {
				if part.filesystem == "efi" {
					"fat"
				} else {
					&part.filesystem
				}
			};

			// Some stupid hackery checks for the args of mkfs.fat
			if part.filesystem == "efi" {
				trace!("mkfs.fat -F32 {devname}");

				cmd_lib::run_cmd!(mkfs.fat -F32 $devname 2>&1)?;
			} else {
				trace!("mkfs.{fsname} {devname}");

				cmd_lib::run_cmd!(mkfs.$fsname $devname 2>&1)?;
			}

			// create partition
			trace!("====================");
		}

		Ok(())
	}
}

#[test]
fn test_partlay() {
	use std::str::FromStr;
	use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
	use tracing_subscriber::Layer;

	// Partition layout test
	let subscriber =
		tracing_subscriber::Registry::default().with(tracing_error::ErrorLayer::default()).with(
			tracing_subscriber::fmt::layer()
				.pretty()
				.with_filter(tracing_subscriber::EnvFilter::from_str("trace").unwrap()),
		);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

	let mock_disk = PathBuf::from("/dev/sda");

	let mut partlay = PartitionLayout::new();

	partlay.add_partition(Partition {
		label: Some("EFI".to_string()),
		size: Some(ByteSize::mib(100)),
		filesystem: "efi".to_string(),
		mountpoint: "/boot/efi".to_string(),
	});

	partlay.add_partition(Partition {
		label: Some("boot".to_string()),
		size: Some(ByteSize::gib(100)),
		filesystem: "ext4".to_string(),
		mountpoint: "/boot".to_string(),
	});

	partlay.add_partition(Partition {
		label: Some("ROOT".to_string()),
		size: Some(ByteSize::gib(100)),
		filesystem: "ext4".to_string(),
		mountpoint: "/".to_string(),
	});

	for (i, part) in partlay.partitions.iter().enumerate() {
		println!("Partition {i}:");
		println!("{part:#?}");

		// get index of partition
		let index = partlay.get_index(&part.mountpoint).unwrap();
		println!("Index: {index}");

		println!("Partition name: {}", partition_name(&mock_disk.to_string_lossy(), index));

		println!("====================");
	}

	let lay = partlay.sort_partitions();

	println!("{:#?}", partlay);
	println!("sorted: {:#?}", lay);

	// Assert that:

	// 1. The partitions are sorted by mountpoint
	// / will come first
	// /boot will come second
	// /boot/efi will come last

	let assertion = vec![
		(
			3,
			Partition {
				label: Some("ROOT".to_string()),
				size: Some(ByteSize::gib(100)),
				filesystem: "ext4".to_string(),
				mountpoint: "/".to_string(),
			},
		),
		(
			2,
			Partition {
				label: Some("boot".to_string()),
				size: Some(ByteSize::gib(100)),
				filesystem: "ext4".to_string(),
				mountpoint: "/boot".to_string(),
			},
		),
		(
			1,
			Partition {
				label: Some("EFI".to_string()),
				size: Some(ByteSize::mib(100)),
				filesystem: "efi".to_string(),
				mountpoint: "/boot/efi".to_string(),
			},
		),
	];

	assert_eq!(lay, assertion)

	// partlay.apply(&mock_disk).unwrap();
	// check if parts would be applied correctly
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Partition {
	pub label: Option<String>,
	// If not specified, the partition will be created at the end of the disk (100%)
	pub size: Option<ByteSize>,
	/// Filesystem of the partition
	pub filesystem: String,
	/// The mountpoint of the partition
	pub mountpoint: String,
}

#[test]
fn test_bytesize() {
	use std::str::FromStr;

	let size = ByteSize::mib(100);
	println!("{size:#?}");

	let size = ByteSize::from_str("100M").unwrap();
	println!("{:#?}", size.as_u64())
}

fn _default_true() -> bool {
	true
}

/// Image default users configuration
#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Auth {
	/// Username for the user
	pub username: String,
	/// Passwords are optional, but heavily recommended
	/// Passwords must be hashed with crypt(3) or mkpasswd(1)
	pub password: Option<String>,
	/// Groups to add the user to
	#[serde(default)]
	pub groups: Vec<String>,
	/// Whether to create a home directory for the user
	/// Defaults to true
	#[serde(default = "_default_true")]
	pub create_home: bool,
	/// Shell for the user
	#[serde(default)]
	pub shell: Option<String>,
	/// UID for the user
	#[serde(default)]
	pub uid: Option<u32>,
	/// GID for the user
	#[serde(default)]
	pub gid: Option<u32>,

	/// SSH keys for the user
	/// This will be written to ~/.ssh/authorized_keys
	#[serde(default)]
	pub ssh_keys: Vec<String>,
}

impl Auth {
	pub fn add_to_chroot(&self, chroot: &Path) -> Result<()> {
		// add user to chroot

		let mut args = vec![];

		if let Some(uid) = self.uid {
			args.push("-u".to_string());
			let binding = uid.to_string();
			args.push(binding.to_string());
		}
		if let Some(gid) = self.gid {
			args.push("-g".to_string());
			args.push(gid.to_string());
		}

		if let Some(shell) = &self.shell {
			args.push("-s".to_string());
			args.push(shell.to_string());
		}

		if let Some(password) = &self.password {
			args.push("-p".to_string());
			args.push(password.to_string());
		}

		if self.create_home {
			args.push("-m".to_string());
		} else {
			args.push("-M".to_string());
		}

		// add groups
		for group in &self.groups {
			args.push("-G".to_string());
			args.push(group.to_string());
		}

		args.push(self.username.to_owned());

		trace!(?args, "useradd args");

		chroot_run_cmd!(chroot, unshare -R $chroot useradd $[args] 2>&1)?;

		// add ssh keys
		if !self.ssh_keys.is_empty() {
			let mut ssh_dir = PathBuf::from(chroot);
			ssh_dir.push("home");
			ssh_dir.push(&self.username);
			ssh_dir.push(".ssh");

			fs::create_dir_all(&ssh_dir)?;

			let mut auth_keys = ssh_dir.clone();
			auth_keys.push("authorized_keys");

			let mut auth_keys_file = fs::File::create(auth_keys)?;

			for key in &self.ssh_keys {
				auth_keys_file.write_all(key.as_bytes())?;
				auth_keys_file.write_all(b"\n")?;
			}
		}

		Ok(())
	}
}

// #[test]
// fn test_recurse() {
// 	// cd tests/ng/recurse

// 	let manifest = Manifest::load_all(PathBuf::from("tests/ng/recurse/manifest.yaml")).unwrap();

// 	println!("{manifest:#?}");

// 	// let ass: Manifest = Manifest { import: vec!["recurse1.yaml", "recurse2.yaml"], distro: Some("RecursiveOS"), out_file: None, dnf: (), scripts: () }
// }
