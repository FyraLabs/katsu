use bytesize::ByteSize;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace};

use crate::cmd;

/// Represents GPT partition attrbite flags which can be used, from https://uapi-group.org/specifications/specs/discoverable_partitions_specification/#partition-attribute-flags.
#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionFlag {
	/// Disable auto discovery for the partition, preventing automatic mounting
	NoAuto,
	/// Mark partition for mounting as read-only
	ReadOnly,
	/// Enable automatically growing the underlying file system when mounted
	GrowFs,
	/// An arbitrary GPT attribute flag position, 0 - 63
	#[serde(untagged)]
	FlagPosition(u8),
}
impl PartitionFlag {
	/// Get the position offset for this flag
	fn flag_position(&self) -> u8 {
		// https://uapi-group.org/specifications/specs/discoverable_partitions_specification/#partition-attribute-flags
		match &self {
			Self::NoAuto => 63,
			Self::ReadOnly => 60,
			Self::GrowFs => 59,
			Self::FlagPosition(position @ 0..=63) => *position,
			_ => unimplemented!(),
		}
	}
}

/// Represents GPT partition types which can be used, a subset of https://uapi-group.org/specifications/specs/discoverable_partitions_specification.
/// If the partition type you need isn't in the enum, please file an issue and use the GUID variant.
/// This is not the filesystem which is formatted on the partition.
#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionType {
	// TODO: we need a global arch option in Katsu
	/// Root partition for the target architecture of the build if set, otherwise defaults to the local architecture
	Root,
	/// Root partition for ARM64
	RootArm64,
	/// Root partition for `x86_64`
	RootX86_64,
	/// Efi system partition
	Esp,
	/// Extended boot loader, defined by the Boot Loader Specification
	Xbootldr,
	/// Swap partition
	Swap,
	/// A generic partition that carries a Linux filesystem
	LinuxGeneric,
	/// An arbitrary GPT partition type GUID/UUIDv4
	#[serde(untagged)]
	Guid(uuid::Uuid),
}

impl PartitionType {
	/// Get the GPT partition type GUID
	fn uuid(&self, target_arch: &str) -> String {
		// https://uapi-group.org/specifications/specs/discoverable_partitions_specification/#partition-names
		match self {
			Self::Root => {
				return match target_arch {
					"x86_64" => Self::RootX86_64.uuid(target_arch),
					"aarch64" => Self::RootArm64.uuid(target_arch),
					_ => unimplemented!(),
				}
			},
			Self::RootArm64 => "b921b045-1df0-41c3-af44-4c6f280d3fae",
			Self::RootX86_64 => "4f68bce3-e8cd-4db1-96e7-fbcaf984b709",
			Self::Esp => "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
			Self::Xbootldr => "bc13c2ff-59e6-4262-a352-b275fd6f7172",
			Self::Swap => "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f",
			Self::LinuxGeneric => "0fc63daf-8483-4772-8e79-3d69d8477de4",
			Self::Guid(guid) => return guid.to_string(),
		}
		.to_string()
	}
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BtrfsSubvolume {
	pub name: String,
	pub mountpoint: String,
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Partition {
	pub label: Option<String>,
	/// Partition type
	#[serde(rename = "type")]
	pub partition_type: PartitionType,
	/// GPT partition attribute flags to add
	// todo: maybe represent this as a bitflag number, parted consumes the positions so I'm doing this for now
	pub flags: Option<Vec<PartitionFlag>>,
	/// If not specified, the partition will be created at the end of the disk (100%)
	pub size: Option<bytesize::ByteSize>,
	/// Filesystem of the partition
	pub filesystem: String,
	/// The mountpoint of the partition
	// todo: make this optional so we can have partitions that aren't mounted
	// and also btrfs subvolumes
	pub mountpoint: String,

	/// Will only be used if the filesystem is btrfs
	#[serde(default)]
	pub subvolumes: Vec<BtrfsSubvolume>,
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct PartitionLayout {
	pub size: Option<bytesize::ByteSize>,
	pub partitions: Vec<Partition>,
}
impl PartitionLayout {
	/// Generate fstab entries for the partitions
	pub fn fstab(&self, chroot: &Path) -> Result<String> {
		// sort partitions by mountpoint
		let ordered = self.sort_partitions();

		crate::prepend_comment!(PREPEND: "/etc/fstab", "static file system information.", katsu::config::PartitionLayout::fstab);

		let mut entries = vec![];

		ordered.iter().try_for_each(|(_, part)| -> Result<()> {
			let mp = PathBuf::from(&part.mountpoint).to_string_lossy().to_string();
			let mountpoint_chroot = part.mountpoint.trim_start_matches('/');
			let mountpoint_chroot = chroot.join(mountpoint_chroot);
			let devname = cmd!(stdout "findmnt" "-n" "-o" "SOURCE" mountpoint_chroot);

			// We will generate by UUID
			let uuid = cmd!(stdout "blkid" "-s" "UUID" "-o" "value" { devname.trim_end() });

			// clean the mountpoint so we don't have the slash at the start
			// let mp_cleaned = part.mountpoint.trim_start_matches('/');

			let fsname = if part.filesystem == "efi" { "vfat" } else { &part.filesystem };
			let fsck = if part.filesystem == "efi" { 0 } else { 2 };

			entries.push(TplFstabEntry { uuid, mp, fsname, fsck });
			Ok(())
		})?;

		tracing::trace!(?entries, "fstab entries generated");

		Ok(crate::tpl!("../../templates/fstab.tera" => { PREPEND, entries }))
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

			tracing::trace!(?index, ?part, "Index and partition");
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

	fn get_index(&self, mountpoint: &str) -> Option<usize> {
		// index should be +1 of the actual partition number (sda1 is index 0)
		self.partitions.iter().position(|p| p.mountpoint == mountpoint).map(|i| i + 1)
	}

	#[deprecated(note = "Switch to systemd-repart")]
	pub fn apply(&self, disk: &PathBuf, target_arch: &str) -> Result<()> {
		// This is a destructive operation, so we need to make sure we don't accidentally wipe the wrong disk

		info!("Applying partition layout to disk: {disk:#?}");

		// format disk with GPT

		debug!("Formatting disk with GPT");
		cmd!(?"parted" "-s" {{ disk.display() }} "mklabel" "gpt")?;

		// create partitions
		self.partitions.iter().try_fold((1, 0), |(i, mut last_end), part| {
			let devname = partition_name(&disk.to_string_lossy(), i);
			trace!(devname, "Creating partition {i}: {part:#?}");

			let span = tracing::trace_span!("partition", devname);
			let _enter = span.enter();

			let start_string = if i == 1 {
				// create partition at start of disk
				"1MiB".to_string()
			} else {
				// create partition after last partition
				ByteSize::b(last_end).to_string_as(true).replace(' ', "")
			};

			let end_string = part.size.map_or("100%".to_string(), |size| {
				// create partition with size
				last_end += size.as_u64();

				// remove space for partition table
				ByteSize::b(last_end).to_string_as(true).replace(' ', "")
			});

			// TODO: primary/extended/logical is a MBR concept, since we're using GPT, we should be using this field to set the label
			// not going to change this for now though, but will revisit
			debug!(start = start_string, end = end_string, "Creating partition");
			cmd!(? "parted" "-s" {{ disk.display() }} "mkpart" "primary" "fat32" start_string end_string)?;

			let part_type_uuid = part.partition_type.uuid(target_arch);

			debug!("Setting partition type");
			trace!("parted -s {disk:?} type {i} {part_type_uuid}");

			if let Some(flags) = &part.flags {
				debug!("Setting partition attribute flags");

				for flag in flags {
					let position = flag.flag_position();
					cmd!(? "sgdisk" "-A" ["{i}:set:{position}"] {{ disk.display() }})?;
				}
			}

			if part.filesystem == "efi" {
				debug!("Setting esp on for efi partition");
				cmd!(? "parted" "-s" {{ disk.display() }} "set" ["{i}"] "esp" "on")?;
			}

			if let Some(label) = &part.label {
				debug!(label, "Setting label");
				cmd!(? "parted" "-s" {{ disk.display() }} "name" ["{i}"] label)?;
			}

			trace!("Refreshing partition tables");
			let _ = cmd!(? "partprobe"); // comes with parted supposedly

			// time to format the filesystem
			let fsname = &part.filesystem;
			// Some stupid hackery checks for the args of mkfs.fat
			debug!(fsname, "Formatting partition");
			if fsname == "efi" {
				trace!("mkfs.fat -F32 {devname}");
				cmd_lib::run_cmd!(mkfs.fat -F32 $devname 2>&1)?;
			} else {
				trace!("mkfs.{fsname} {devname}");
				cmd_lib::run_cmd!(mkfs.$fsname $devname 2>&1)?;
			}

			Result::<_>::Ok((i + 1, last_end))
		})?;

		Ok(())
	}

	// todo: move to tiffin::Container
	#[deprecated(note = "use tiffin::Container instead")]
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

	pub fn unmount_from_chroot(&self, chroot: &Path) -> Result<()> {
		// unmount partitions from chroot
		// sort partitions by mountpoint
		for mp in self.sort_partitions().into_iter().rev().map(|(_, p)| p.mountpoint) {
			let mp = chroot.join(mp.trim_start_matches('/'));
			trace!("umount {mp:?}");
			cmd_lib::run_cmd!(umount $mp 2>&1)?;
		}
		Ok(())
	}
}

#[derive(Serialize, Debug)]
struct TplFstabEntry<'a> {
	uuid: String,
	mp: String,
	fsname: &'a str,
	fsck: u8,
}

/// Utility function for determining partition /dev names
/// For cases where it's a mmcblk, or nvme, or loop device etc
#[must_use]
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
/// An ISO9660 partition for an ISO9660 image
#[derive(Clone, Debug)]
pub struct Iso9660Partition {
    pub partno: usize,
    /// UUID for partition type
    pub guid: PartitionType,
}

/// A partition table for an ISO9660 image
#[derive(Clone, Debug)]
pub struct Iso9660Table {}

/// A wrapper around xorriso
#[derive(Debug, Clone)]
pub struct Xorriso {
    /// Implant MD5 checksums?
    /// default: true
    pub md5: bool,
    /// Boot catalog
    pub boot_catalog: Option<PathBuf>,
    
}
