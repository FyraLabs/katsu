use serde::{Deserialize, Serialize};

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
	/// Root partition for x86_64
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
			PartitionType::Root => {
				return match target_arch {
					"x86_64" => PartitionType::RootX86_64.uuid(target_arch),
					"aarch64" => PartitionType::RootArm64.uuid(target_arch),
					_ => unimplemented!(),
				}
			},
			PartitionType::RootArm64 => "b921b045-1df0-41c3-af44-4c6f280d3fae",
			PartitionType::RootX86_64 => "4f68bce3-e8cd-4db1-96e7-fbcaf984b709",
			PartitionType::Esp => "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
			PartitionType::Xbootldr => "bc13c2ff-59e6-4262-a352-b275fd6f7172",
			PartitionType::Swap => "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f",
			PartitionType::LinuxGeneric => "0fc63daf-8483-4772-8e79-3d69d8477de4",
			PartitionType::Guid(guid) => return guid.to_string(),
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
