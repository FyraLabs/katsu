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
IgnoreOnIsolate=yes
Wants=systemd-udev-trigger.service
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

// These mounts are created by prepare-root, not by .mount unit ExecStart.
// Keep their runtime-discovered units out of ordinary local-fs teardown and
// order them before the target retained by initrd-switch-root.target.
const OSTREE_BIND_MOUNT_DROPIN: &str =
	"[Unit]\nDefaultDependencies=no\nIgnoreOnIsolate=yes\nBefore=initrd-fs.target\n";

const PREPARE_ROOT_DROPIN: &str = r#"[Service]
ExecStartPre=/usr/libexec/katsu-ostree-mountpoints
"#;

const MOUNTPOINTS: &str = r#"#!/bin/sh
. /lib/dracut-lib.sh

fail() {
    echo "katsu-ostree-mountpoints: $*" >&2
    exit 1
}

# getarg calls debug_on, which may return 1: do not invoke it under errexit.
ostree_path=$(getarg ostree=) || fail 'Missing ostree deployment boot link'
deployment=$(realpath -e "/sysroot$ostree_path") \
    || fail "Cannot resolve deployment boot link: /sysroot$ostree_path"
case "$deployment" in
    /sysroot/ostree/deploy/*/deploy/*) ;;
    *) fail "Invalid deployment after sysroot mount: $deployment" ;;
esac
mkdir -p "$deployment/sysroot" "$deployment/var" \
    || fail "Cannot create mountpoints in deployment: $deployment"
echo "katsu-ostree-mountpoints: prepared mountpoints in $deployment"
# prepare-root creates /run/ostree itself. The real-root OSTree generator
# mounts the stateroot's var from the writable physical sysroot.
"#;

const PREPARE_ROOT_CONFIG: &str = "[composefs]\nenabled=false\n[root]\ntransient=false\n[sysroot]\nreadonly=false\n[etc]\ntransient=false\n";

/// Composefs equivalent of [`LIVE_SERVICE`].
///
/// Root selection belongs to bootc's `bootc-root-setup.service`, which reads
/// `composefs=<D>`; our only job is to make the ISO the backing filesystem and
/// provide a writable upper for the repository.
const COMPOSEFS_LIVE_SERVICE: &str = r#"[Unit]
Description=Prepare Katsu composefs live media
DefaultDependencies=no
ConditionKernelCommandLine=rd.katsu.composefs
IgnoreOnIsolate=yes
Wants=systemd-udev-trigger.service
After=systemd-udev-trigger.service dracut-pre-mount.service
Before=sysroot.mount
OnFailure=emergency.target
OnFailureJobMode=isolate

[Service]
Type=oneshot
ExecStart=/usr/libexec/katsu-composefs-live
RemainAfterExit=yes
StandardOutput=journal+console
StandardError=journal+console
"#;

/// `/sysroot` for a composefs boot: the physical media merged with a writable
/// upper, so bootc can open the repository and write its runtime state.
const COMPOSEFS_SYSROOT_MOUNT: &str = r#"[Unit]
Description=Katsu writable composefs live sysroot
DefaultDependencies=no
Requires=katsu-composefs-live.service
After=katsu-composefs-live.service
Before=initrd-root-fs.target
OnFailure=emergency.target
OnFailureJobMode=isolate

[Mount]
What=overlay
Where=/sysroot
Type=overlay
Options=lowerdir=/run/katsu/ro,upperdir=/run/katsu/writable/upper,workdir=/run/katsu/writable/work
"#;

/// Stage the composefs initramfs integration.
///
/// Appended to the image's own initramfs rather than regenerated with dracut: a
/// composefs tree has no `usr/` at its root, so dracut has nothing to work from,
/// and the image already ships an initramfs built for its kernel. Appending
/// keeps that and layers our media handling on top.
pub fn stage_composefs_live(workspace: &Path) -> Result<PathBuf> {
	let stage = workspace.join("composefs-live-initramfs");
	if stage.exists() {
		fs::remove_dir_all(&stage)?;
	}
	let files = [
		("usr/libexec/katsu-composefs-live", include_str!("katsu-composefs-live.sh"), true),
		(
			"usr/lib/systemd/system-generators/katsu-composefs-generator",
			include_str!("katsu-composefs-generator.sh"),
			true,
		),
		("usr/lib/systemd/system/katsu-composefs-live.service", COMPOSEFS_LIVE_SERVICE, false),
		("usr/lib/systemd/system/sysroot.mount", COMPOSEFS_SYSROOT_MOUNT, false),
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
			"usr/lib/systemd/system/sysroot-usr.mount.d/katsu-live.conf",
			OSTREE_BIND_MOUNT_DROPIN,
			false,
		),
		(
			"usr/lib/systemd/system/sysroot-sysroot.mount.d/katsu-live.conf",
			OSTREE_BIND_MOUNT_DROPIN,
			false,
		),
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
	append_stage(initramfs, workspace, &stage, "ostree-live")
}

/// Append the composefs integration to an initramfs the image already provides.
pub fn append_composefs_live(initramfs: &Path, workspace: &Path) -> Result<()> {
	let stage = stage_composefs_live(workspace)?;
	append_stage(initramfs, workspace, &stage, "composefs-live")
}

fn append_stage(
	initramfs: &Path, workspace: &Path, stage: &Path, archive_name: &str,
) -> Result<()> {
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
	collect(stage, stage, &mut paths)?;
	let archive = workspace.join(format!("{archive_name}.cpio"));
	let mut cpio = Command::new("cpio")
		.args(["--create", "--format=newc", "--null", "--owner=0:0", "--reproducible", "--quiet"])
		.current_dir(stage)
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
		bail!("Failed to create the {archive_name} initramfs integration archive");
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
	fn stages_composefs_live_handoff_for_bootc_root_setup() {
		let workspace =
			std::env::temp_dir().join(format!("katsu-initramfs-cfs-{}", uuid::Uuid::new_v4()));
		let stage = stage_composefs_live(&workspace).unwrap();

		// bootc assembles the root, so we must not ship the OSTree prepare-root
		// overrides or disable composefs for this layout.
		assert!(!stage.join("etc/ostree/prepare-root.conf").exists());
		assert!(
			!stage
				.join("usr/lib/systemd/system/ostree-prepare-root.service.d/katsu-live.conf")
				.exists()
		);

		let service =
			fs::read_to_string(stage.join("usr/lib/systemd/system/katsu-composefs-live.service"))
				.unwrap();
		// Ordering is what keeps the mounts alive through switch-root, so assert the
		// same protections the OSTree path needed.
		assert!(service.contains("ConditionKernelCommandLine=rd.katsu.composefs"));
		assert!(service.contains("IgnoreOnIsolate=yes"));
		assert!(service.contains("Wants=systemd-udev-trigger.service"));
		assert!(!service.contains("Requires=systemd-udev-trigger.service"));
		assert!(service.contains("Before=sysroot.mount"));

		let script = "usr/libexec/katsu-composefs-live";
		assert_eq!(fs::metadata(stage.join(script)).unwrap().permissions().mode() & 0o111, 0o111);
		assert!(
			std::process::Command::new("sh")
				.arg("-n")
				.arg(stage.join(script))
				.status()
				.unwrap()
				.success()
		);
		// The live service must be pinned to the labeled ISO device, or it races
		// udev and cannot resolve /dev/disk/by-label/<label>.
		let generator = "usr/lib/systemd/system-generators/katsu-composefs-generator";
		assert_eq!(
			fs::metadata(stage.join(generator)).unwrap().permissions().mode() & 0o111,
			0o111
		);
		assert!(
			std::process::Command::new("sh")
				.arg("-n")
				.arg(stage.join(generator))
				.status()
				.unwrap()
				.success()
		);
		assert!(
			stage
				.join("usr/lib/systemd/system/initrd-root-fs.target.requires/sysroot.mount")
				.exists()
		);
		fs::remove_dir_all(workspace).unwrap();
	}

	#[test]
	fn mountpoints_helper_tolerates_dracut_debug_on_returning_false() {
		let workspace = std::env::temp_dir().join(format!("katsu-getarg-{}", uuid::Uuid::new_v4()));
		fs::create_dir_all(&workspace).unwrap();
		let library = workspace.join("dracut-lib.sh");
		fs::write(
			&library,
			r#"
debug_on() { [ "$RD_DEBUG" = yes ] && set -x; }
getarg() {
    echo /ostree/boot.1/um/checksum/0
    debug_on
    return 0
}
"#,
		)
		.unwrap();
		let script =
			MOUNTPOINTS.replace(". /lib/dracut-lib.sh", &format!(". {}", library.display()));
		// Stub filesystem commands so the test needs no privileged mounts.
		let script = format!(
			"realpath() {{ echo /sysroot/ostree/deploy/um/deploy/checksum.0; }}\nmkdir() {{ return 0; }}\n{script}"
		);
		let mut child = Command::new("sh")
			.env_remove("RD_DEBUG")
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.unwrap();
		child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
		let output = child.wait_with_output().unwrap();
		assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
		assert!(String::from_utf8_lossy(&output.stdout).contains("prepared mountpoints"));
		fs::remove_dir_all(workspace).unwrap();
	}

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
		let live_service =
			fs::read_to_string(stage.join("usr/lib/systemd/system/katsu-ostree-live.service"))
				.unwrap();
		assert!(live_service.contains("IgnoreOnIsolate=yes"));
		assert!(live_service.contains("Wants=systemd-udev-trigger.service"));
		assert!(!live_service.contains("Requires=systemd-udev-trigger.service"));
		for unit in ["sysroot-usr.mount", "sysroot-sysroot.mount"] {
			let dropin = stage.join(format!("usr/lib/systemd/system/{unit}.d/katsu-live.conf"));
			let contents = fs::read_to_string(dropin).unwrap();
			assert!(contents.contains("DefaultDependencies=no"));
			assert!(contents.contains("IgnoreOnIsolate=yes"));
		}
		// Repeated incremental builds must not retain old integration files.
		fs::write(stage.join("stale"), b"old").unwrap();
		stage_ostree_live(&workspace).unwrap();
		assert!(!stage.join("stale").exists());
		fs::remove_dir_all(workspace).unwrap();
	}
}
