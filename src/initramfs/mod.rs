use color_eyre::{Result, eyre::bail};
use std::{
	fs,
	io::Write,
	os::unix::fs::PermissionsExt,
	path::{Path, PathBuf},
	process::{Command, Stdio},
};

const LIVE_SERVICE: &str = r#"[Unit]
Description=Prepare Katsu OSTree live media
DefaultDependencies=no
ConditionKernelCommandLine=rd.katsu.ostree
Requires=systemd-udev-trigger.service
After=systemd-udev-trigger.service dracut-pre-mount.service
Before=sysroot.mount
OnFailure=emergency.target
OnFailureJobMode=isolate

[Service]
Type=oneshot
ExecStart=/usr/libexec/katsu-ostree-live
RemainAfterExit=yes
StandardOutput=journal+console
StandardError=journal+console
"#;

const SYSROOT_MOUNT: &str = r#"[Unit]
Description=Katsu writable OSTree live sysroot
DefaultDependencies=no
Requires=katsu-ostree-live.service
After=katsu-ostree-live.service
Before=initrd-root-fs.target
OnFailure=emergency.target
OnFailureJobMode=isolate

[Mount]
What=overlay
Where=/sysroot
Type=overlay
Options=lowerdir=/run/katsu/ro,upperdir=/run/katsu/writable/upper,workdir=/run/katsu/writable/work
"#;

const PREPARE_ROOT_DROPIN: &str = r#"[Service]
ExecStartPre=/usr/libexec/katsu-ostree-mountpoints
"#;

const MOUNTPOINTS: &str = r#"#!/bin/sh
. /lib/dracut-lib.sh
set -e
ostree_path=$(getarg ostree=)
deployment=$(realpath -e "/sysroot$ostree_path")
case "$deployment" in
    /sysroot/ostree/deploy/*/deploy/*) ;;
    *) echo 'katsu: invalid deployment after sysroot mount' >&2; exit 1 ;;
esac
mkdir -p "$deployment/sysroot" "$deployment/var"
# prepare-root creates /run/ostree itself. The real-root OSTree generator
# mounts the stateroot's var from the writable physical sysroot.
"#;

const PREPARE_ROOT_CONFIG: &str = "[composefs]\nenabled=false\n[root]\ntransient=false\n[sysroot]\nreadonly=false\n[etc]\ntransient=false\n";

/// Stage an initramfs-only integration, leaving the source deployment untouched.
/// Appended as a separate CPIO so overrides win without dracut's --sysroot
/// source-path rewriting or changes to immutable deployment directories.
pub fn stage_ostree_live(workspace: &Path) -> Result<PathBuf> {
	let stage = workspace.join("ostree-live-initramfs");
	if stage.exists() {
		fs::remove_dir_all(&stage)?;
	}
	let files = [
		("usr/libexec/katsu-ostree-live", include_str!("katsu-ostree-live.sh"), true),
		("usr/libexec/katsu-ostree-mountpoints", MOUNTPOINTS, true),
		(
			"usr/lib/systemd/system-generators/katsu-ostree-generator",
			include_str!("katsu-ostree-generator.sh"),
			true,
		),
		("usr/lib/systemd/system/katsu-ostree-live.service", LIVE_SERVICE, false),
		("usr/lib/systemd/system/sysroot.mount", SYSROOT_MOUNT, false),
		(
			"usr/lib/systemd/system/ostree-prepare-root.service.d/katsu-live.conf",
			PREPARE_ROOT_DROPIN,
			false,
		),
		("etc/ostree/prepare-root.conf", PREPARE_ROOT_CONFIG, false),
		(
			"var/lib/dracut/hooks/cmdline/99-katsu-ostree.sh",
			"#!/bin/sh\nif getargbool 0 rd.katsu.ostree; then\n    root=katsu-ostree\n    rootok=1\nfi\n",
			true,
		),
	];
	for (relative, contents, executable) in files {
		let path = stage.join(relative);
		fs::create_dir_all(path.parent().unwrap())?;
		fs::write(&path, contents)?;
		if executable {
			fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
		}
	}
	let wants = stage.join("usr/lib/systemd/system/initrd-root-fs.target.requires");
	fs::create_dir_all(&wants)?;
	std::os::unix::fs::symlink("../sysroot.mount", wants.join("sysroot.mount"))?;
	Ok(stage)
}

/// Linux unpacks concatenated initramfs archives in order, including an
/// uncompressed newc archive after dracut's compressed archive.
pub fn append_ostree_live(initramfs: &Path, workspace: &Path) -> Result<()> {
	let stage = stage_ostree_live(workspace)?;
	let mut paths = Vec::new();
	fn collect(dir: &Path, stage: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
		let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
		entries.sort_by_key(|entry| entry.file_name());
		for entry in entries {
			paths.push(entry.path().strip_prefix(stage)?.to_path_buf());
			if entry.file_type()?.is_dir() {
				collect(&entry.path(), stage, paths)?;
			}
		}
		Ok(())
	}
	collect(&stage, &stage, &mut paths)?;
	let archive = workspace.join("ostree-live.cpio");
	let mut cpio = Command::new("cpio")
		.args(["--create", "--format=newc", "--null", "--owner=0:0", "--reproducible", "--quiet"])
		.current_dir(&stage)
		.stdin(Stdio::piped())
		.stdout(fs::File::create(&archive)?)
		.spawn()?;
	let mut input = cpio.stdin.take().unwrap();
	for path in paths {
		input.write_all(path.as_os_str().as_encoded_bytes())?;
		input.write_all(&[0])?;
	}
	drop(input);
	if !cpio.wait()?.success() {
		bail!("Failed to create OSTree live initramfs integration archive");
	}
	let mut output = fs::OpenOptions::new().append(true).open(initramfs)?;
	let padding = (4 - output.metadata()?.len() % 4) % 4;
	output.write_all(&[0; 3][..padding as usize])?;
	std::io::copy(&mut fs::File::open(&archive)?, &mut output)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn appended_archive_extracts_live_integration() {
		let workspace = std::env::temp_dir().join(format!("katsu-cpio-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&workspace).unwrap();
		let image = workspace.join("initramfs.img");
		fs::write(&image, b"").unwrap();
		append_ostree_live(&image, &workspace).unwrap();
		let extracted = workspace.join("extracted");
		fs::create_dir(&extracted).unwrap();
		assert!(
			Command::new("cpio")
				.args(["--extract", "--make-directories", "--quiet"])
				.current_dir(&extracted)
				.stdin(fs::File::open(&image).unwrap())
				.status()
				.unwrap()
				.success()
		);
		assert_eq!(
			fs::read_to_string(extracted.join("etc/ostree/prepare-root.conf")).unwrap(),
			PREPARE_ROOT_CONFIG
		);
		assert!(
			extracted
				.join("usr/lib/systemd/system/initrd-root-fs.target.requires/sysroot.mount")
				.exists()
		);
		assert_ne!(
			fs::metadata(extracted.join("usr/libexec/katsu-ostree-live"))
				.unwrap()
				.permissions()
				.mode() & 0o111,
			0
		);
		fs::remove_dir_all(workspace).unwrap();
	}

	#[test]
	fn stages_live_root_with_ordered_ostree_handoff() {
		let workspace =
			std::env::temp_dir().join(format!("katsu-initramfs-{}", uuid::Uuid::new_v4()));
		let stage = stage_ostree_live(&workspace).unwrap();
		assert!(
			fs::read_to_string(stage.join("etc/ostree/prepare-root.conf"))
				.unwrap()
				.contains("enabled=false")
		);
		assert!(
			fs::read_to_string(stage.join("usr/lib/systemd/system/sysroot.mount"))
				.unwrap()
				.contains("After=katsu-ostree-live.service")
		);
		assert!(
			stage
				.join("usr/lib/systemd/system/initrd-root-fs.target.requires/sysroot.mount")
				.exists()
		);
		for script in [
			"usr/libexec/katsu-ostree-live",
			"usr/libexec/katsu-ostree-mountpoints",
			"usr/lib/systemd/system-generators/katsu-ostree-generator",
			"var/lib/dracut/hooks/cmdline/99-katsu-ostree.sh",
		] {
			assert_eq!(
				fs::metadata(stage.join(script)).unwrap().permissions().mode() & 0o111,
				0o111
			);
			assert!(
				std::process::Command::new("sh")
					.arg("-n")
					.arg(stage.join(script))
					.status()
					.unwrap()
					.success()
			);
		}
		// Repeated incremental builds must not retain old integration files.
		fs::write(stage.join("stale"), b"old").unwrap();
		stage_ostree_live(&workspace).unwrap();
		assert!(!stage.join("stale").exists());
		fs::remove_dir_all(workspace).unwrap();
	}
}
