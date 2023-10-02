use bytesize::ByteSize;
use color_eyre::Result;
use merge_struct::merge;
use serde_derive::{Deserialize, Serialize};
use std::{path::PathBuf, str::FromStr, fs};
use tracing::{debug, trace, info};

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

	#[serde(default)]
	pub scripts: ScriptsManifest,
}

impl Manifest {
	/// Loads a single manifest from a file
	pub fn load(path: PathBuf) -> Result<Self> {
		let mut manifest: Self = serde_yaml::from_str(&std::fs::read_to_string(path.clone())?)?;

		// get dir of path relative to cwd

		let mut path_can = path.canonicalize()?;

		path_can.pop();
		trace!(path = ?path_can, "Canonicalizing path");

		for import in &mut manifest.import {
			debug!("Import: {import:#?}");
			// swap canonicalized path
			let cn = path_can.join(&import).canonicalize()?;
			debug!("Canonicalized import: {cn:#?}");
			*import = cn;
		}

		// canonicalize all file paths in scripts, then modify their paths put in the manifest

		for script in &mut manifest.scripts.pre {
			if let Some(f) = script.file.as_mut() {
				trace!(f = ?f, "Loading Script file");
				let cn = path_can.join(&f).canonicalize()?;
				*f = cn;
			}
		}

		for script in &mut manifest.scripts.post {
			if let Some(f) = script.file.as_mut() {
				trace!(f = ?f, "Loading script file");
				let cn = path_can.join(&f).canonicalize()?;
				*f = cn;
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

	pub fn load_all(path: PathBuf) -> Result<Self> {
		// get all imports, then merge them all
		let mut manifest = Self::load(path.clone())?;

		// get dir of path

		let mut path_can = path.clone();
		path_can.pop();

		for import in manifest.import.clone() {
			let imported_manifest = Self::load_all(import.clone())?;
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

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
// load script from file, or inline if there's one specified
pub struct Script {
	pub id: Option<String>,
	pub name: Option<String>,
	pub file: Option<PathBuf>,
	pub inline: Option<String>,
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
pub fn partition_name(disk: &str, partition: u32) -> String {
	let devname = if disk.starts_with("/dev/mmcblk") {
		// mmcblk0p1
		format!("{}p{}", disk, partition)
	} else if disk.starts_with("/dev/nvme") {
		// nvme0n1p1
		format!("{}p{}", disk, partition)
	} else if disk.starts_with("/dev/loop") {
		// loop0p1
		format!("{}p{}", disk, partition)
	} else {
		// sda1
		format!("{}{}", disk, partition)
	};

	devname
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

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PartitionLayout {
	pub size: Option<ByteSize>,
	pub partitions: Vec<Partition>,
}

impl PartitionLayout {
	pub fn new() -> Self {
		Self { partitions: Vec::new(), size: None }
	}

	/// Adds a partition to the layout
	pub fn add_partition(&mut self, partition: Partition) {
		self.partitions.push(partition);
	}

	pub fn get_index(&self, mountpoint: &str) -> Option<usize> {
		// index should be +1 of the actual partition number (sda1 is index 0)

		Some(
			self.partitions.iter().position(|p| p.mountpoint == mountpoint).unwrap_or_default() + 1,
		)
	}

	pub fn get_partition(&self, mountpoint: &str) -> Option<&Partition> {
		self.partitions.iter().find(|p| p.mountpoint == mountpoint)
	}

	pub fn sort_partitions(&mut self) {
		// We should sort partitions by mountpoint, so that we can mount them in order
		// In this case, from the least nested to the most nested, so count the number of slashes

		self.partitions.sort_by(|a, b| {
			let a_slashes = a.mountpoint.matches('/').count();
			let b_slashes = b.mountpoint.matches('/').count();

			a_slashes.cmp(&b_slashes)
		});
	}

	pub fn mount_to_chroot(&self, disk: &PathBuf, chroot: &PathBuf) -> Result<()> {
		// mount partitions to chroot

		// sort partitions by mountpoint
		let mut ordered = self.clone();
		ordered.sort_partitions();

		debug!(or = ?ordered, "Mounting partitions to chroot");

		for part in &ordered.partitions {
			let index = ordered.get_index(&part.mountpoint).unwrap();
			let devname = partition_name(&disk.to_string_lossy(), index as u32);

			// clean the mountpoint so we don't have the slash at the start
			let mp_cleaned = part.mountpoint.trim_start_matches('/');
			let mountpoint = chroot.join(&mp_cleaned);

			trace!(
				"mount {devname} {mountpoint}",
				devname = devname,
				mountpoint = mountpoint.to_string_lossy()
			);

			fs::create_dir_all(&mountpoint)?;

			cmd_lib::run_cmd!(mount ${devname} ${mountpoint} 2>&1)?;
		}

		Ok(())
	}

	pub fn unmount_from_chroot(&self, disk: &PathBuf, chroot: &PathBuf) -> Result<()> {
		// unmount partitions from chroot

		// sort partitions by mountpoint
		let mut ordered = self.clone();
		ordered.sort_partitions();
		// reverse the order
		ordered.partitions.reverse();

		debug!(or = ?ordered, "Unmounting partitions from chroot");

		for part in &ordered.partitions {
			let index = ordered.get_index(&part.mountpoint).unwrap();
			let devname = partition_name(&disk.to_string_lossy(), index as u32);

			// clean the mountpoint so we don't have the slash at the start
			let mp_cleaned = part.mountpoint.trim_start_matches('/');
			let mountpoint = chroot.join(&mp_cleaned);

			trace!(
				"umount {devname} {mountpoint}",
				devname = devname,
				mountpoint = mountpoint.to_string_lossy()
			);

			cmd_lib::run_cmd!(umount -l ${mountpoint} 2>&1)?;
		}

		Ok(())
	}

	/// Generate fstab entries for the partitions
	pub fn fstab(&self, chroot: &PathBuf) -> Result<String> {
		// sort partitions by mountpoint
		let mut ordered = self.clone();
		ordered.sort_partitions();

		let mut fstab = String::new();

		for part in &ordered.partitions {

			// get devname by finding from mount, instead of index because we won't be using it as much
			let mountpoint = PathBuf::from(&part.mountpoint);
			let mountpoint_chroot = part.mountpoint.trim_start_matches('/');
			let mountpoint_chroot = chroot.join(mountpoint_chroot);

			debug!(mountpoint = ?mountpoint, "Mountpoint of partition");
			debug!(mountpoint_chroot = ?mountpoint_chroot, "Mountpoint of partition in chroot");

			let devname = cmd_lib::run_fun!(findmnt -n -o SOURCE ${mountpoint_chroot})?;


			debug!(devname = ?devname, "Device name of partition");

			// We will generate by UUID

			let uuid = cmd_lib::run_fun!(blkid -s UUID -o value ${devname})?;

			debug!(uuid = ?uuid, "UUID of partition");

			// clean the mountpoint so we don't have the slash at the start
			// let mp_cleaned = part.mountpoint.trim_start_matches('/');

			let fsname = {
				if part.filesystem == "efi" {
					"vfat"
				} else {
					&part.filesystem
				}
			};

			let fsck = if part.filesystem == "efi" { "0" } else { "2" };

			let entry = format!(
				"UUID={uuid}\t{mountpoint}\t{fsname}\tdefaults\t0\t{fsck}",
				uuid = uuid,
				mountpoint = mountpoint.to_string_lossy(),
				fsname = fsname,
				fsck = fsck
			);

			fstab.push_str(&entry);
			fstab.push('\n');
		}

		Ok(fstab)
	}

	pub fn apply(&self, disk: &PathBuf) -> Result<()> {
		// This is a destructive operation, so we need to make sure we don't accidentally wipe the wrong disk

		info!("Applying partition layout to disk: {:#?}", disk);

		// format disk with GPT

		trace!("Formatting disk with GPT");
		trace!("parted -s {disk} mklabel gpt", disk = disk.to_string_lossy());
		cmd_lib::run_cmd!(parted -s ${disk} mklabel gpt 2>&1)?;

		// create partitions

		let mut last_end = 0;

		for (i, part) in self.partitions.iter().enumerate() {
			trace!("Creating partition {}:", i + 1);
			trace!("{:#?}", part);

			// get index of partition
			let index = self.get_index(&part.mountpoint).unwrap();
			trace!("Index: {}", index);

			let devname = partition_name(&disk.to_string_lossy(), index as u32);

			let start_string = if i == 0 {
				// create partition at start of disk
				"1MiB".to_string()
			} else {
				// create partition after last partition
				ByteSize::b(last_end).to_string_as(true).replace(" ", "")
			};

			let end_string = if let Some(size) = part.size {
				// create partition with size
				last_end += size.as_u64();

				// remove space for partition table
				ByteSize::b(last_end).to_string_as(true).replace(" ", "")
			} else {
				// create partition at end of disk
				"100%".to_string()
			};

			let part_fs_parted = if part.filesystem == "efi" { "fat32" } else { "ext4" };

			trace!(
				"parted -s {disk} mkpart primary {part_fs} {start} {end}",
				disk = disk.to_string_lossy(),
				part_fs = part_fs_parted,
				start = start_string,
				end = end_string
			);

			cmd_lib::run_cmd!(parted -s ${disk} mkpart primary ${part_fs_parted} ${start_string} ${end_string} 2>&1)?;

			if part.filesystem == "efi" {
				trace!(
					"parted -s {disk} set {index} esp on",
					disk = disk.to_string_lossy(),
					index = index
				);

				cmd_lib::run_cmd!(parted -s ${disk} set ${index} esp on 2>&1)?;
			}
			

			if let Some(label) = &part.label {
				trace!(
					"parted -s {disk} name {index} {label}",
					disk = disk.to_string_lossy(),
					index = index,
					label = label
				);

				cmd_lib::run_cmd!(parted -s ${disk} name ${index} ${label} 2>&1)?;
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
				trace!(
					"mkfs.fat -F32 {devname}",
					devname = devname
				);

				cmd_lib::run_cmd!(mkfs.fat -F32 ${devname} 2>&1)?;
			} else {
				trace!(
					"mkfs.{fs} {devname}",
					fs = fsname,
					devname = devname
				);

				cmd_lib::run_cmd!(mkfs.${fsname} ${devname} 2>&1)?;
			}

			// create partition
			trace!("====================");
		}

		Ok(())
	}
}

#[test]
fn test_partlay() {
	// Partition layout test

	let mock_disk = PathBuf::from("/dev/sda");

	let mut partlay = PartitionLayout::new();

	partlay.add_partition(Partition {
		label: Some("EFI".to_string()),
		size: Some(ByteSize::mib(100)),
		filesystem: "efi".to_string(),
		mountpoint: "/boot/efi".to_string(),
	});

	partlay.add_partition(Partition {
		label: Some("ROOT".to_string()),
		size: Some(ByteSize::gib(100)),
		filesystem: "ext4".to_string(),
		mountpoint: "/".to_string(),
	});

	partlay.add_partition(Partition {
		label: Some("HOME".to_string()),
		size: Some(ByteSize::gib(100)),
		filesystem: "ext4".to_string(),
		mountpoint: "/home".to_string(),
	});

	for (i, part) in partlay.partitions.iter().enumerate() {
		println!("Partition {}:", i);
		println!("{:#?}", part);

		// get index of partition
		let index = partlay.get_index(&part.mountpoint).unwrap();
		println!("Index: {}", index);

		println!("Partition name: {}", partition_name(&mock_disk.to_string_lossy(), index as u32));

		println!("====================");
	}

	partlay.apply(&mock_disk).unwrap();
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
	let size = ByteSize::mib(100);
	println!("{:#?}", size);

	let size = ByteSize::from_str("100M").unwrap();
	println!("{:#?}", size.as_u64())
}
// #[test]
// fn test_recurse() {
// 	// cd tests/ng/recurse

// 	let manifest = Manifest::load_all(PathBuf::from("tests/ng/recurse/manifest.yaml")).unwrap();

// 	println!("{manifest:#?}");

// 	// let ass: Manifest = Manifest { import: vec!["recurse1.yaml", "recurse2.yaml"], distro: Some("RecursiveOS"), out_file: None, dnf: (), scripts: () }
// }
