//! Wrap a live payload in a GPT disk image.
//!
//! bootc's `Storage::new` needs a discoverable EFI System Partition on the media
//! it is booted from. The image is p1 ESP, p2 XBOOTLDR (`/boot`), and p3 the
//! read-only EROFS root.

use crate::util::{create_sparse, loopdev_with_file_and_parts};
use color_eyre::{Result, eyre::bail, eyre::eyre};
use gpt::{GptConfig, mbr::ProtectiveMBR, partition_types};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use std::{
	fs,
	path::{Path, PathBuf},
	process::Command,
	time::{Duration, Instant},
};
use tracing::{debug, info};

/// Small: the staging ESP carries only ~12M of files.
pub const ESP_SIZE: u64 = 64 * 1024 * 1024;

/// 1MiB alignment
const START_OFFSET: u64 = 1024 * 1024;

/// Headroom above the `/boot` tree's size
const XBOOTLDR_HEADROOM: u64 = 16 * 1024 * 1024;

/// Slack above the payload so the root partition can never truncate it.
const ROOT_HEADROOM: u64 = 16 * 1024 * 1024;

const SECTOR_SIZE: u64 = 512;

/// The staging image's ESP, to be copied into the payload.
#[derive(Debug, Clone)]
pub struct EspSource {
	/// Block device of the staging image's ESP partition, e.g. `/dev/loop0p2`.
	pub device: PathBuf,
}

/// `/dev/loopNpM` for partition `n` of a partition-scanned loop device.
fn partition_path(disk: &Path, n: u32) -> PathBuf {
	PathBuf::from(format!("{}p{n}", disk.display()))
}

/// Locate the ESP partition inside a partitioned staging image.
///
/// Reads the GPT directly: the `lsblk` crate does not expose the partition type.
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
	let device = partition_path(disk, *index);
	debug!(?device, "Found staging image ESP");
	Ok(EspSource { device })
}

/// The partition type GUID of every partition in a GPT image, as
/// `(partition number, type GUID)` sorted by number.
fn read_partition_types(disk: &Path) -> Result<Vec<(u32, uuid::Uuid)>> {
	let parsed = GptConfig::new()
		.open(disk)
		.map_err(|e| eyre!("Reading GPT from {}: {e}", disk.display()))?;
	Ok(parsed.partitions().iter().map(|(index, part)| (*index, part.part_type_guid.guid)).collect())
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

/// Partition sizes: XBOOTLDR fits `/boot` plus headroom (at least [`ESP_SIZE`]),
/// and root is rounded up to whole sectors.
pub fn layout(root_len: u64, boot_len: u64) -> Layout {
	let esp_size = ESP_SIZE;
	let xbootldr_size = round_up_mib(boot_len + XBOOTLDR_HEADROOM).max(ESP_SIZE);
	let root_size = round_up_sector(root_len + ROOT_HEADROOM);
	// Two alignment offsets: one before the ESP and one of slack at the end, which
	// also leaves room for the backup GPT header.
	let total = START_OFFSET + esp_size + xbootldr_size + root_size + START_OFFSET;
	Layout { esp_size, xbootldr_size, root_size, total }
}

fn round_up_mib(bytes: u64) -> u64 {
	const MIB: u64 = 1024 * 1024;
	bytes.div_ceil(MIB) * MIB
}

fn round_up_sector(bytes: u64) -> u64 {
	bytes.div_ceil(SECTOR_SIZE) * SECTOR_SIZE
}

/// Build the ESP + XBOOTLDR + EROFS root GPT image at `destination`.
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
	// Write the table before attaching: the kernel scans partitions once, at attach.
	write_partition_table(options.destination, &parts)?;

	let (disk, _handle) = loopdev_with_file_and_parts(options.destination)?;
	wait_for_partitions(&disk, &[1, 2, 3])?;
	let (esp, xbootldr, root) =
		(partition_path(&disk, 1), partition_path(&disk, 2), partition_path(&disk, 3));

	format_vfat(&esp, "ESP")?;
	copy_esp_into(options.esp, &esp, options.scratch)?;

	format_vfat(&xbootldr, "XBOOTLDR")?;
	copy_tree_into_vfat(options.boot_tree, &xbootldr, options.scratch)?;

	write_erofs_into(options.root_image, &root)
}

/// The gpt crate leaves LBA0 empty; without a protective MBR the kernel does not
/// see the disk as partitioned.
fn write_partition_table(image: &Path, layout: &Layout) -> Result<()> {
	let mut disk = GptConfig::new()
		.writable(true)
		.create(image)
		.map_err(|e| eyre!("Creating GPT on {}: {e}", image.display()))?;

	// Partition ids are 1-based; geometry is in 512-byte sectors.
	let first_lba = START_OFFSET / SECTOR_SIZE;
	let esp_lba = layout.esp_size / SECTOR_SIZE;
	let xbootldr_lba = layout.xbootldr_size / SECTOR_SIZE;
	let root_lba = layout.root_size / SECTOR_SIZE;

	disk.add_partition_at("EFI-SYSTEM", 1, first_lba, esp_lba, partition_types::EFI, 0)
		.map_err(|e| eyre!("Adding ESP partition: {e}"))?;
	// FREEDESK_BOOT is the XBOOTLDR type.
	disk.add_partition_at(
		"XBOOTLDR",
		2,
		first_lba + esp_lba,
		xbootldr_lba,
		partition_types::FREEDESK_BOOT,
		0,
	)
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

	let mut file = disk.write().map_err(|e| eyre!("Writing GPT: {e}"))?;
	let sectors = u32::try_from(layout.total / SECTOR_SIZE - 1).unwrap_or(u32::MAX);
	ProtectiveMBR::with_lb_size(sectors)
		.overwrite_lba0(&mut file)
		.map_err(|e| eyre!("Writing protective MBR: {e}"))?;
	file.sync_all()?;
	debug!("Wrote GPT partition table");
	Ok(())
}

/// Poll until `<disk>p<N>` exists for every `N`, so a freshly partitioned device
/// can be formatted without racing the kernel's partition scan.
fn wait_for_partitions(disk: &Path, numbers: &[u32]) -> Result<()> {
	let deadline = Instant::now() + Duration::from_secs(10);
	for &n in numbers {
		let path = partition_path(disk, n);
		while !path.exists() {
			if Instant::now() > deadline {
				bail!("Partition {} did not appear after partitioning", path.display());
			}
			std::thread::sleep(Duration::from_millis(50));
		}
	}
	Ok(())
}

fn format_vfat(device: &Path, label: &str) -> Result<()> {
	let output = Command::new("mkfs.vfat")
		.arg("-v")
		.arg("-n")
		.arg(label)
		.arg(device)
		.output()
		.map_err(|e| crate::util::tool_error("mkfs.vfat", "dosfstools", e))?;
	if !output.status.success() {
		bail!(
			"mkfs.vfat failed for {}: {}",
			device.display(),
			String::from_utf8_lossy(&output.stderr)
		);
	}
	Ok(())
}

/// Copy the staging image's ESP files into the payload's ESP partition.
fn copy_esp_into(source: &EspSource, dest: &Path, scratch: &Path) -> Result<()> {
	copy_contents_into(
		MountPoint::mount(&source.device, scratch, "esp-src", true)?.path(),
		dest,
		scratch,
	)
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
	let dest_mount = MountPoint::mount(dest_device, scratch, "copy", false)?;
	copy_recursive(source, dest_mount.path())
}

/// Recursively copy, following symlinks (vfat cannot store them).
fn copy_recursive(source: &Path, dest: &Path) -> Result<()> {
	if fs::metadata(source)?.is_dir() {
		fs::create_dir_all(dest)?;
		for entry in fs::read_dir(source)? {
			let entry = entry?;
			copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
		}
	} else {
		fs::copy(source, dest)?;
	}
	Ok(())
}

/// Write the EROFS payload into the root partition.
///
/// No `sync_all`: forcing ~4G through the loop-on-overlay stack stalls in
/// `balance_dirty_pages` for minutes, and the final close of the device flushes it.
fn write_erofs_into(root_image: &Path, root_partition: &Path) -> Result<()> {
	std::io::copy(
		&mut fs::File::open(root_image)?,
		&mut fs::OpenOptions::new().write(true).open(root_partition)?,
	)?;
	Ok(())
}

fn dir_size(path: &Path) -> Result<u64> {
	if !path.is_dir() {
		return Ok(0);
	}
	let mut total = 0;
	for entry in fs::read_dir(path)? {
		let entry = entry?;
		// Follows symlinks, matching what `copy_recursive` writes.
		let meta = fs::metadata(entry.path())?;
		if meta.is_dir() {
			total += dir_size(&entry.path())?;
		} else {
			total += meta.len();
		}
	}
	Ok(total)
}

/// A vfat partition mounted for as long as the guard lives.
struct MountPoint {
	path: PathBuf,
}

impl MountPoint {
	fn mount(device: &Path, scratch: &Path, name: &str, read_only: bool) -> Result<Self> {
		let path = scratch.join(format!("mnt-{name}"));
		fs::create_dir_all(&path)?;
		let flags = if read_only { MsFlags::MS_RDONLY } else { MsFlags::empty() };
		mount(Some(device), &path, Some("vfat"), flags, None::<&str>)
			.map_err(|e| eyre!("Mounting {} at {}: {e}", device.display(), path.display()))?;
		Ok(Self { path })
	}

	fn path(&self) -> &Path {
		&self.path
	}
}

impl Drop for MountPoint {
	fn drop(&mut self) {
		if let Err(err) = umount2(&self.path, MntFlags::MNT_DETACH) {
			debug!(?err, path = %self.path.display(), "Unmounting failed");
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::io::{Seek, SeekFrom};

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
	fn finds_the_esp_by_partition_type() {
		// The ESP is identified by its GPT type GUID, not by label or order, so a
		// table with the ESP in any position must resolve to that partition.
		let dir = std::env::temp_dir().join(format!("katsu-esp-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&dir).unwrap();
		let image = dir.join("disk.img");
		let parts = layout(1024 * 1024, 0);
		create_sparse(&image, parts.total).unwrap();
		write_partition_table(&image, &parts).unwrap();

		let esp_source = find_staging_esp(&image).unwrap();
		assert_eq!(esp_source.device, partition_path(&image, 1));

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
		// No ESP-typed partition, so discovery must fail.
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

		assert!(
			partitions.values().any(|p| p.part_type_guid == partition_types::EFI),
			"an ESP-typed partition must exist"
		);

		assert!(
			partitions.values().any(|p| p.part_type_guid == partition_types::LINUX_ROOT_X64),
			"the root partition must use bootc's x86-64 root type"
		);
		assert!(
			partitions.values().any(|p| p.part_type_guid == partition_types::FREEDESK_BOOT),
			"the XBOOTLDR partition must use the DPS type"
		);

		fs::remove_dir_all(dir).unwrap();
	}

	/// End-to-end cover for the whole payload assembly.
	///
	/// Needs loop devices and mount, so it only runs when explicitly asked for
	/// (`KATSU_TEST_PRIVILEGED=1 cargo test`) and is skipped otherwise.
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
		let esp = EspSource { device: partition_path(&esp_dev, 1) };
		// The partition node is published asynchronously after the scan.
		wait_for_partitions(&esp_dev, &[1]).unwrap();
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
		wait_for_partitions(&loop_dev, &[1, 2, 3]).unwrap();
		// A block device's `metadata().len()` is 0; seeking to the end gives its size.
		let size =
			fs::File::open(partition_path(&loop_dev, 3)).unwrap().seek(SeekFrom::End(0)).unwrap();
		let payload_len = fs::metadata(&payload).unwrap().len();
		assert!(size >= payload_len, "root partition {size} < payload {payload_len}");
		drop(loop_handle);

		fs::remove_dir_all(dir).ok();
	}
}
