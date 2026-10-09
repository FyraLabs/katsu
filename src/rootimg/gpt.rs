//! Wrap a live payload in a GPT disk image.
//!
//! bootc's `Storage::new` needs a discoverable EFI System Partition on the media
//! it is booted from — for every command, including `bootc status`. It walks
//! `lsblk --inverse` from the device backing `/sysroot` up to the whole disk and
//! looks for a child with the ESP partition type:
//!
//! ```text
//! let root_dev = bootc_blockdev::list_dev_by_dir(&physical_root)?;
//! let esp_dev = root_dev.find_first_colocated_esp()?;   // hard error if absent
//! ```
//!
//! A bare EROFS loop over a file on ISO9660 cannot satisfy that: `lsblk` does not
//! model "file on a mounted filesystem" as a parent, so the loop is treated as
//! the root device, and a bare EROFS loop has no children.
//!
//! Presenting the payload as a partitioned disk fixes the walk. The layout
//! mirrors what `bootc install to-disk` produces, with the root filesystem
//! replaced by our read-only EROFS:
//!
//! ```text
//! rootfs.img (GPT)
//!  ├─ p1  ESP       64M     vfat    c12a7328-f81f-11d2-ba4b-00a0c93ec93b
//!  ├─ p2  XBOOTLDR  ~       vfat    bc13c2ff-59e6-4262-a352-b275fd6f7172
//!  └─ p3  root      payload erofs   4f68bce3-e8cd-4db1-96e7-fbcaf984b709
//! ```
//!
//! Only the partition *types* matter for discovery; `mount_esp_readonly` then
//! mounts p1 for real, so it has to be a genuine vfat filesystem.

use crate::util::{create_sparse, loopdev_with_file_and_parts};
use color_eyre::{Result, eyre::bail, eyre::eyre};
use gpt::{GptConfig, partition_types};
use std::{
	fs,
	path::{Path, PathBuf},
	process::{Command, Stdio},
	time::{Duration, Instant},
};
use tracing::{debug, info};

/// Extended Boot Loader Partition, per the Discoverable Partitions Specification.
/// The `gpt` crate has no constant for this one.
const XBOOTLDR_GUID: &str = "bc13c2ff-59e6-4262-a352-b275fd6f7172";

/// Size of the ESP written into the payload. The staging image's ESP is 1G but
/// only carries ~12M of bootloader files, and the whole payload is compressed
/// into the ISO, so a small partition keeps the artifact from growing for no
/// reason.
pub const ESP_SIZE: u64 = 64 * 1024 * 1024;

/// Conventional 1MiB alignment for the start of the first partition.
const START_OFFSET: u64 = 1024 * 1024;

/// Headroom above the `/boot` tree's size for the vfat filesystem's own metadata.
const XBOOTLDR_HEADROOM: u64 = 16 * 1024 * 1024;

/// The staging image's ESP, to be copied into the payload.
#[derive(Debug, Clone)]
pub struct EspSource {
	/// Block device of the staging image's ESP partition, e.g. `/dev/loop0p2`.
	pub device: PathBuf,
}

/// Locate the ESP partition inside a partitioned staging image.
///
/// The table is read straight from the image with the `gpt` crate rather than by
/// shelling out: the partition *type* only exists in the GPT, and the `lsblk`
/// crate does not expose it (it has `partuuid`/`partlabel`, not `parttype`).
pub fn find_staging_esp(disk: &Path) -> Result<EspSource> {
	let parts = read_partition_types(disk)?;
	let (index, _) =
		parts.iter().find(|(_, guid)| *guid == partition_types::EFI.guid).ok_or_else(|| {
			eyre!(
				"No ESP partition in the staging image {}; bootc's composefs install \
				 should have created one",
				disk.display()
			)
		})?;

	// The caller attaches the staging image as a partition-scanning loop device, so
	// the ESP is published as `<loop>p<index>`.
	let device = PathBuf::from(format!("{}p{index}", disk.display()));
	debug!(?device, "Found staging image ESP");
	Ok(EspSource { device })
}

/// The partition type GUID of every partition in a GPT image, as
/// `(partition number, type GUID)` sorted by number.
fn read_partition_types(disk: &Path) -> Result<Vec<(u32, uuid::Uuid)>> {
	let parsed = GptConfig::new()
		.open(disk)
		.map_err(|e| eyre!("Reading GPT from {}: {e}", disk.display()))?;
	let mut parts: Vec<(u32, uuid::Uuid)> = parsed
		.partitions()
		.iter()
		.map(|(index, part)| (*index, part.part_type_guid.guid))
		.collect();
	parts.sort_by_key(|(index, _)| *index);
	Ok(parts)
}

/// Inputs for [`wrap_in_gpt`].
pub struct WrapOptions<'a> {
	/// The EROFS payload that becomes the root partition.
	pub root_image: &'a Path,
	/// Where the finished GPT image is written. Must differ from `root_image`.
	pub destination: &'a Path,
	/// Directory whose contents become the XBOOTLDR partition's `/boot` tree.
	pub boot_tree: &'a Path,
	/// The staging image's ESP, copied into the ESP partition.
	pub esp: &'a EspSource,
	/// Directory for scratch mountpoints, on a real filesystem.
	pub scratch: &'a Path,
}

/// Partition geometry for a payload of `root_len` bytes plus `boot_len` of
/// `/boot` content.
#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
	pub esp_size: u64,
	pub xbootldr_size: u64,
	pub root_size: u64,
	pub total: u64,
}

/// Headroom above the payload for the root partition.
///
/// Deliberately non-zero: sizing the partition to exactly the EROFS length leaves
/// no margin, so the copy would silently truncate the moment the image grew by a
/// sector. A few MiB costs nothing on a multi-gigabyte payload.
const ROOT_HEADROOM: u64 = 16 * 1024 * 1024;

/// Compute the payload's partition sizes.
///
/// The XBOOTLDR partition is sized from the `/boot` tree with headroom for the
/// filesystem's metadata, rounded up to the next MiB and never smaller than the
/// ESP, so a nearly-empty `/boot` still leaves a usable partition. The root size
/// is rounded up to a whole sector, since the partition table is expressed in
/// sectors and a truncated size would leave the payload short of its EROFS image.
pub fn layout(root_len: u64, boot_len: u64) -> Layout {
	let esp_size = ESP_SIZE;
	let xbootldr_size = round_up_mib(boot_len + XBOOTLDR_HEADROOM).max(ESP_SIZE);
	let root_size = round_up_sector(root_len + ROOT_HEADROOM);
	// Two alignment offsets: one before the ESP and one of slack at the end, which
	// also leaves room for the backup GPT header.
	let total = START_OFFSET + esp_size + xbootldr_size + root_size + START_OFFSET;
	Layout { esp_size, xbootldr_size, root_size, total }
}

const SECTOR_SIZE: u64 = 512;

fn round_up_mib(bytes: u64) -> u64 {
	const MIB: u64 = 1024 * 1024;
	bytes.div_ceil(MIB) * MIB
}

fn round_up_sector(bytes: u64) -> u64 {
	bytes.div_ceil(SECTOR_SIZE) * SECTOR_SIZE
}

/// Build the GPT payload described in the module docs.
pub fn wrap_in_gpt(options: &WrapOptions) -> Result<()> {
	if options.root_image == options.destination {
		bail!("The GPT destination must differ from the EROFS payload it contains");
	}
	let root_len = fs::metadata(options.root_image)?.len();
	let boot_len = dir_size(options.boot_tree)?;
	let parts = layout(root_len, boot_len);
	info!(
		esp = parts.esp_size,
		xbootldr = parts.xbootldr_size,
		root = parts.root_size,
		total = parts.total,
		"Wrapping the EROFS payload in a GPT disk image"
	);

	fs::create_dir_all(options.scratch)?;
	create_sparse(options.destination, parts.total)?;
	// The table must be written before the loop is attached: rewriting the header
	// of an already-attached loop does not make the kernel re-read it, so the
	// partition nodes never appear. Attaching a complete table scans it once.
	write_partition_table(options.destination, &parts)?;

	let (disk, handle) = loopdev_with_file_and_parts(options.destination)?;
	let parts_dev = PartitionDevices::new(&disk);
	parts_dev.wait_ready()?;

	format_vfat(&parts_dev.esp, "ESP")?;
	copy_esp_into(options.esp, &parts_dev.esp, options.scratch)?;

	format_vfat(&parts_dev.xbootldr, "XBOOTLDR")?;
	copy_tree_into_vfat(options.boot_tree, &parts_dev.xbootldr, options.scratch)?;

	write_erofs_into(options.root_image, &parts_dev.root)?;

	drop(handle);
	Ok(())
}

/// Write the partition table with the `gpt` crate, so no external tool is needed
/// and the kernel sees a fully-formed table the first time the image is attached.
///
/// The crate does not write the protective MBR at LBA0 on its own, and without it
/// neither the kernel's partition scanner nor `sfdisk` recognises the disk as
/// partitioned at all.
fn write_partition_table(image: &Path, layout: &Layout) -> Result<()> {
	let mut disk = GptConfig::new()
		.writable(true)
		.create(image)
		.map_err(|e| eyre!("Creating GPT on {}: {e}", image.display()))?;

	let xbootldr = partition_types::Type::from(uuid_guid(XBOOTLDR_GUID)?);

	// Partition ids are 1-based; geometry is in 512-byte sectors.
	let first_lba = START_OFFSET / SECTOR_SIZE;
	let esp_lba = layout.esp_size / SECTOR_SIZE;
	let xbootldr_lba = layout.xbootldr_size / SECTOR_SIZE;
	let root_lba = layout.root_size / SECTOR_SIZE;

	disk.add_partition_at("EFI-SYSTEM", 1, first_lba, esp_lba, partition_types::EFI, 0)
		.map_err(|e| eyre!("Adding ESP partition: {e}"))?;
	disk.add_partition_at("XBOOTLDR", 2, first_lba + esp_lba, xbootldr_lba, xbootldr, 0)
		.map_err(|e| eyre!("Adding XBOOTLDR partition: {e}"))?;
	disk.add_partition_at(
		"root",
		3,
		first_lba + esp_lba + xbootldr_lba,
		root_lba,
		partition_types::LINUX_ROOT_X64,
		0,
	)
	.map_err(|e| eyre!("Adding root partition: {e}"))?;

	disk.write().map_err(|e| eyre!("Writing GPT: {e}"))?;

	write_protective_mbr(image, layout.total)?;

	let total_sectors = layout.total / SECTOR_SIZE;
	debug!(total_sectors, "Wrote GPT partition table");
	Ok(())
}

/// Write the protective MBR that marks the disk as GPT-partitioned.
///
/// The single partition covers the whole disk with type `0xEE`, which is what
/// stops tools that only understand MBR from treating the disk as blank.
fn write_protective_mbr(image: &Path, total_bytes: u64) -> Result<()> {
	use std::io::{Seek, SeekFrom, Write};

	let sectors = u32::try_from(total_bytes / SECTOR_SIZE).unwrap_or(u32::MAX);
	// A protective MBR partition starts at LBA 1 and spans the disk, capped at the
	// 32-bit MBR size field.
	let size = sectors.saturating_sub(1).min(u32::MAX - 1);

	let mut mbr = [0u8; 512];
	let entry = &mut mbr[446..462];
	entry[0] = 0x00; // not bootable
	entry[1..4].copy_from_slice(&[0x00, 0x02, 0x00]); // CHS start (unused)
	entry[4] = 0xEE; // GPT protective
	entry[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]); // CHS end (unused)
	entry[8..12].copy_from_slice(&1u32.to_le_bytes()); // first LBA
	entry[12..16].copy_from_slice(&size.to_le_bytes());
	mbr[510] = 0x55;
	mbr[511] = 0xAA;

	let mut file = fs::OpenOptions::new()
		.write(true)
		.open(image)
		.map_err(|e| eyre!("Opening {} for the protective MBR: {e}", image.display()))?;
	file.seek(SeekFrom::Start(0))?;
	file.write_all(&mbr)?;
	file.sync_all()?;
	Ok(())
}

fn uuid_guid(guid: &str) -> Result<uuid::Uuid> {
	uuid::Uuid::parse_str(guid).map_err(|e| eyre!("Invalid GUID {guid}: {e}"))
}

/// The `/dev/loopNpN` nodes of a partitioned loop device.
struct PartitionDevices {
	disk: PathBuf,
	esp: PathBuf,
	xbootldr: PathBuf,
	root: PathBuf,
}

impl PartitionDevices {
	fn new(disk: &Path) -> Self {
		let part = |n: u32| PathBuf::from(format!("{}p{n}", disk.display()));
		Self { disk: disk.to_path_buf(), esp: part(1), xbootldr: part(2), root: part(3) }
	}

	/// Wait for the kernel to publish the partition nodes after the table is read.
	fn wait_ready(&self) -> Result<()> {
		wait_for_partitions(&self.disk, &[1, 2, 3]);
		for path in [&self.esp, &self.xbootldr, &self.root] {
			if !path.exists() {
				bail!("Partition {} did not appear after partitioning", path.display());
			}
		}
		Ok(())
	}
}

/// Poll until `/dev/<disk>p<N>` exists for every `N`, so a freshly partitioned
/// device can be formatted without racing the kernel's partition scan.
pub(crate) fn wait_for_partitions(disk: &Path, numbers: &[u32]) {
	for n in numbers {
		let path = PathBuf::from(format!("{}p{n}", disk.display()));
		let deadline = Instant::now() + Duration::from_secs(10);
		while !path.exists() {
			if Instant::now() > deadline {
				debug!(path = %path.display(), "Timed out waiting for partition node");
				return;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
	}
}

fn format_vfat(device: &Path, label: &str) -> Result<()> {
	let output = Command::new("mkfs.vfat")
		.arg("-v")
		.arg("-n")
		.arg(label)
		.arg(device)
		.output()
		.map_err(|e| eyre!("Running mkfs.vfat: {e}"))?;
	if !output.status.success() {
		bail!(
			"mkfs.vfat failed for {}: {}",
			device.display(),
			String::from_utf8_lossy(&output.stderr)
		);
	}
	Ok(())
}

/// Copy the staging image's ESP contents into the payload's ESP partition.
///
/// Copying the files rather than the raw filesystem image keeps the destination
/// sized to what we reserved; the ESP is much smaller than the staging one.
fn copy_esp_into(source: &EspSource, dest: &Path, scratch: &Path) -> Result<()> {
	let source_mount = MountPoint::mount_ro(&source.device, scratch, "esp-src")?;
	let result = copy_contents_into(source_mount.path(), dest, scratch);
	drop(source_mount);
	result
}

/// Copy the `/boot` tree into the XBOOTLDR partition.
fn copy_tree_into_vfat(tree: &Path, dest: &Path, scratch: &Path) -> Result<()> {
	if !tree.is_dir() {
		debug!(?tree, "No /boot tree to copy into XBOOTLDR");
		return Ok(());
	}
	copy_contents_into(tree, dest, scratch)
}

/// Copy every entry of `source` into the vfat filesystem on `dest_device`.
fn copy_contents_into(source: &Path, dest_device: &Path, scratch: &Path) -> Result<()> {
	let dest_mount = MountPoint::mount(dest_device, scratch, "copy")?;
	for entry in fs::read_dir(source)? {
		let entry = entry?;
		copy_recursive(&entry.path(), &dest_mount.path().join(entry.file_name()))?;
	}
	Ok(())
}

/// Recursively copy a file or directory. vfat stores no permissions or symlinks,
/// so only contents matter; symlinks are followed, which is what a real install's
/// boot directories contain anyway.
fn copy_recursive(source: &Path, dest: &Path) -> Result<()> {
	let meta = fs::symlink_metadata(source)?;
	let source = if meta.file_type().is_symlink() {
		fs::canonicalize(source)?
	} else {
		source.to_path_buf()
	};
	let meta = fs::metadata(&source)?;
	if meta.is_dir() {
		fs::create_dir_all(dest)?;
		for entry in fs::read_dir(&source)? {
			let entry = entry?;
			copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
		}
	} else {
		fs::copy(&source, dest)?;
	}
	Ok(())
}

/// Write the EROFS payload into the root partition.
///
/// No `sync_all` afterwards: the destination is a loop device whose backing file
/// sits on the workspace, which on a composefs build is itself an overlay over a
/// loop. Forcing the full 4 GiB through that stack parks the process in
/// `balance_dirty_pages` for tens of minutes and gains nothing, because every
/// later reader goes through the same page cache. The write is already visible to
/// them as soon as `io::copy` returns.
fn write_erofs_into(root_image: &Path, root_partition: &Path) -> Result<()> {
	let payload_len = fs::metadata(root_image)?.len();
	let partition_len = block_device_size(root_partition);
	if partition_len > 0 && payload_len > partition_len {
		bail!(
			"Root partition ({} bytes) is smaller than the payload ({} bytes); \
			 the image would be truncated",
			partition_len,
			payload_len
		);
	}

	let mut source = fs::File::open(root_image)?;
	let mut dest = fs::OpenOptions::new().write(true).open(root_partition)?;
	let copied = std::io::copy(&mut source, &mut dest)?;
	if copied != payload_len {
		bail!("Copied {copied} of {payload_len} payload bytes into the root partition");
	}
	Ok(())
}

fn dir_size(path: &Path) -> Result<u64> {
	if !path.is_dir() {
		return Ok(0);
	}
	let mut total = 0;
	for entry in fs::read_dir(path)? {
		let entry = entry?;
		let meta = fs::symlink_metadata(entry.path())?;
		if meta.is_dir() {
			total += dir_size(&entry.path())?;
		} else {
			total += meta.len();
		}
	}
	Ok(total)
}

/// A directory that is mounted for as long as the guard lives.
struct MountPoint {
	path: PathBuf,
}

impl MountPoint {
	fn mount(device: &Path, scratch: &Path, name: &str) -> Result<Self> {
		Self::mount_with(device, scratch, name, false)
	}

	fn mount_ro(device: &Path, scratch: &Path, name: &str) -> Result<Self> {
		Self::mount_with(device, scratch, name, true)
	}

	fn mount_with(device: &Path, scratch: &Path, name: &str, read_only: bool) -> Result<Self> {
		let path = scratch.join(format!("mnt-{name}"));
		fs::create_dir_all(&path)?;
		let mut cmd = Command::new("mount");
		if read_only {
			cmd.arg("-o").arg("ro");
		}
		let status = cmd
			.arg(device)
			.arg(&path)
			.stdout(Stdio::null())
			.status()
			.map_err(|e| eyre!("Mounting {}: {e}", device.display()))?;
		if !status.success() {
			bail!("mount {} at {} failed with {status}", device.display(), path.display());
		}
		Ok(Self { path })
	}

	fn path(&self) -> &Path {
		&self.path
	}
}

impl Drop for MountPoint {
	fn drop(&mut self) {
		if let Err(err) = Command::new("umount").arg("-l").arg(&self.path).status() {
			debug!(?err, path = %self.path.display(), "Unmounting failed");
		}
	}
}

/// Size in bytes of a block device, via the kernel rather than `metadata`, which
/// reports 0 for device nodes. Returns 0 when it cannot be determined, which
/// callers treat as "unknown" rather than "empty".
fn block_device_size(device: &Path) -> u64 {
	let output = Command::new("blockdev").arg("--getsize64").arg(device).output();
	match output {
		Ok(out) if out.status.success() => {
			String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
		},
		_ => 0,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::util::loopdev_with_file_and_parts;

	#[test]
	fn layout_reserves_room_for_every_partition() {
		let root = 4 * 1024 * 1024 * 1024;
		let parts = layout(root, 32 * 1024 * 1024);
		assert_eq!(parts.esp_size, ESP_SIZE);
		assert!(parts.xbootldr_size >= 32 * 1024 * 1024 + XBOOTLDR_HEADROOM);
		// The root partition must exceed the payload, or the copy truncates it.
		assert!(
			parts.root_size > root,
			"root {} is not larger than the payload {root}",
			parts.root_size
		);
		// Every partition plus both alignment gaps must fit in the total.
		assert!(parts.total >= START_OFFSET + parts.esp_size + parts.xbootldr_size + root);
	}

	#[test]
	fn root_partition_has_headroom_over_the_payload() {
		// Sizing the partition to exactly the EROFS length is the failure this
		// guards: `io::copy` stops at the source's EOF, so a payload that grew by a
		// sector would be silently cut short mid-file.
		let payload = 1024 * 1024 + 7;
		let parts = layout(payload, 0);
		assert!(parts.root_size >= payload + ROOT_HEADROOM);
	}

	#[test]
	fn xbootldr_never_smaller_than_the_esp() {
		// An empty /boot must still yield a partition the filesystem can live in.
		let parts = layout(1024, 0);
		assert_eq!(parts.xbootldr_size, ESP_SIZE);
	}

	#[test]
	fn layout_is_sector_aligned() {
		// Non-MiB-aligned inputs must not produce fractional sectors.
		let parts = layout(1234, 5678);
		for value in [parts.esp_size, parts.xbootldr_size, parts.total] {
			assert_eq!(value % 512, 0, "{value} is not sector aligned");
		}
	}

	#[test]
	fn xbootldr_guid_parses() {
		assert!(uuid_guid(XBOOTLDR_GUID).is_ok());
	}

	#[test]
	fn finds_the_esp_by_partition_type() {
		// The ESP is identified by its GPT type GUID, not by label or order, so a
		// table with the ESP in any position must resolve to that partition.
		let dir = std::env::temp_dir().join(format!("katsu-esp-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&dir).unwrap();
		let image = dir.join("disk.img");
		let parts = layout(1024 * 1024, 0);
		create_sparse(&image, parts.total).unwrap();
		write_partition_table(&image, &parts).unwrap();

		let types = read_partition_types(&image).unwrap();
		let esp = types.iter().find(|(_, guid)| *guid == partition_types::EFI.guid);
		assert_eq!(esp.map(|(index, _)| *index), Some(1));

		let esp_source = find_staging_esp(&image).unwrap();
		assert_eq!(esp_source.device, PathBuf::from(format!("{}p1", image.display())));

		fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn reports_no_esp_when_no_partition_has_the_type() {
		// A disk without an ESP must be reported, not silently accepted: bootc would
		// fail later with a much less obvious message.
		let dir = std::env::temp_dir().join(format!("katsu-noesp-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&dir).unwrap();
		let image = dir.join("disk.img");
		let parts = Layout {
			esp_size: 1024 * 1024,
			xbootldr_size: 1024 * 1024,
			root_size: 1024 * 1024,
			total: 8 * 1024 * 1024,
		};
		create_sparse(&image, parts.total).unwrap();
		// Only the ESP+root are written, and neither is ESP-typed here, so discovery
		// must report a miss rather than picking the wrong partition.
		let mut disk = GptConfig::new().writable(true).create(&image).unwrap();
		disk.add_partition_at("root", 1, 2048, 4096, partition_types::LINUX_ROOT_X64, 0).unwrap();
		disk.write().unwrap();

		assert!(find_staging_esp(&image).is_err());

		fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn writes_a_table_bootc_can_walk() {
		// The whole point of the wrapper: the root partition's parent must expose
		// an ESP-typed child, which is what `find_first_colocated_esp` looks for.
		let dir = std::env::temp_dir().join(format!("katsu-gpt-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&dir).unwrap();
		let image = dir.join("rootfs.img");
		let parts = layout(1024 * 1024, 0);
		create_sparse(&image, parts.total).unwrap();
		write_partition_table(&image, &parts).unwrap();

		let disk = GptConfig::new().open(&image).unwrap();
		let partitions = disk.partitions();
		assert_eq!(partitions.len(), 3, "expected ESP, XBOOTLDR and root");

		let esp = partitions
			.values()
			.find(|p| p.part_type_guid == partition_types::EFI)
			.expect("an ESP-typed partition must exist");
		assert_eq!(esp.part_type_guid, partition_types::EFI);

		assert!(
			partitions.values().any(|p| p.part_type_guid == partition_types::LINUX_ROOT_X64),
			"the root partition must use bootc's x86-64 root type"
		);
		assert!(
			partitions.values().any(|p| p.part_type_guid
				== partition_types::Type::from(uuid_guid(XBOOTLDR_GUID).unwrap())),
			"the XBOOTLDR partition must use the DPS type"
		);

		fs::remove_dir_all(dir).unwrap();
	}

	/// End-to-end cover for the whole payload assembly.
	///
	/// Needs loop devices and mount, so it only runs when explicitly asked for
	/// (`KATSU_TEST_PRIVILEGED=1 cargo test`) and is skipped otherwise. It exists
	/// because the wrapper's failure mode in a real build was an argument error in a
	/// helper, which unit tests of the pieces alone did not catch.
	#[test]
	fn assembles_a_payload_bootc_can_discover() {
		if std::env::var("KATSU_TEST_PRIVILEGED").as_deref() != Ok("1") {
			eprintln!("skipping privileged test; set KATSU_TEST_PRIVILEGED=1 to run");
			return;
		}

		let dir = std::env::temp_dir().join(format!("katsu-wrap-{}", uuid::Uuid::new_v4()));
		let scratch = dir.join("scratch");
		fs::create_dir_all(&scratch).unwrap();

		// A stand-in for the EROFS payload.
		let payload = dir.join("payload.erofs");
		fs::write(&payload, vec![0xABu8; 1024 * 1024]).unwrap();

		// A stand-in ESP, partitioned and formatted like the real staging image's.
		// It goes through the same writer the payload uses, since a table without a
		// protective MBR is not recognised by the kernel at all.
		let staging = dir.join("staging.img");
		let staging_parts = Layout {
			esp_size: 8 * 1024 * 1024,
			xbootldr_size: 8 * 1024 * 1024,
			root_size: 8 * 1024 * 1024,
			total: 32 * 1024 * 1024,
		};
		create_sparse(&staging, staging_parts.total).unwrap();
		write_partition_table(&staging, &staging_parts).unwrap();

		let (esp_dev, esp_handle) = loopdev_with_file_and_parts(&staging).unwrap();
		let esp = EspSource { device: PathBuf::from(format!("{}p1", esp_dev.display())) };
		// The partition node is published asynchronously after the scan; formatting
		// before it exists fails with ENOENT.
		wait_for_partitions(&esp_dev, &[1]);
		format_vfat(&esp.device, "ESP").unwrap();

		// A stand-in /boot tree with a nested directory.
		let boot_tree = dir.join("boot");
		fs::create_dir_all(boot_tree.join("loader/entries")).unwrap();
		fs::write(boot_tree.join("loader/entries/entry.conf"), b"title test").unwrap();

		let destination = dir.join("rootfs.img");
		wrap_in_gpt(&WrapOptions {
			root_image: &payload,
			destination: &destination,
			boot_tree: &boot_tree,
			esp: &esp,
			scratch: &scratch,
		})
		.unwrap();

		drop(esp_handle);

		// The assembled image must present the ESP type bootc looks for, plus the
		// XBOOTLDR and root types, and the payload must land in the root partition.
		let types = read_partition_types(&destination).unwrap();
		assert!(types.iter().any(|(_, guid)| *guid == partition_types::EFI.guid));
		assert!(
			types.iter().any(|(_, guid)| *guid == partition_types::LINUX_ROOT_X64.guid),
			"the root partition must use bootc's type"
		);

		let (loop_dev, loop_handle) = loopdev_with_file_and_parts(&destination).unwrap();
		wait_for_partitions(&loop_dev, &[1, 2, 3]);
		// A block device's size is not its `metadata().len()`; ask the kernel.
		let root_part = PathBuf::from(format!("{}p3", loop_dev.display()));
		let size = block_device_size(&root_part);
		let payload_len = fs::metadata(&payload).unwrap().len();
		assert!(size >= payload_len, "root partition {size} < payload {payload_len}");
		drop(loop_handle);

		fs::remove_dir_all(dir).ok();
	}
}
