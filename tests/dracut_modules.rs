use std::{
	fs,
	io::Write,
	os::unix::fs::PermissionsExt,
	path::{Path, PathBuf},
	process::{Command, Stdio},
};

fn module(name: &str) -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("dracut").join(name)
}

fn assert_executable_shell(path: &Path, shell: &str) {
	let mode = fs::metadata(path).unwrap().permissions().mode();
	assert_eq!(mode & 0o111, 0o111, "{} is not executable", path.display());
	assert!(
		Command::new(shell).arg("-n").arg(path).status().unwrap().success(),
		"{} has a syntax error",
		path.display()
	);
}

#[test]
fn module_scripts_are_executable_and_parse() {
	for name in ["90katsu-ostree", "90katsu-composefs"] {
		let dir = module(name);
		assert_executable_shell(&dir.join("module-setup.sh"), "bash");
		for entry in fs::read_dir(&dir).unwrap() {
			let path = entry.unwrap().path();
			if path.extension().is_some_and(|ext| ext == "sh") {
				assert_executable_shell(&path, "sh");
			}
		}
	}
}

#[test]
fn modules_are_opt_in() {
	for name in ["90katsu-ostree", "90katsu-composefs"] {
		let setup = fs::read_to_string(module(name).join("module-setup.sh")).unwrap();
		assert!(setup.contains("return 255"), "{name} must only be included on request");
	}
}

#[test]
fn composefs_module_hands_off_to_bootc_root_setup() {
	let dir = module("90katsu-composefs");
	let setup = fs::read_to_string(dir.join("module-setup.sh")).unwrap();
	assert!(setup.contains("echo bootc systemd"));
	assert!(!setup.contains("prepare-root"), "bootc assembles the root, not ostree-prepare-root");
	assert!(setup.contains("bootc-root-setup.service.wants/katsu-composefs-state.service"));
	assert!(setup.contains("initrd-root-fs.target.requires/sysroot.mount"));

	let service = fs::read_to_string(dir.join("katsu-composefs-live.service")).unwrap();
	assert!(service.contains("ConditionKernelCommandLine=rd.katsu.composefs"));
	assert!(service.contains("IgnoreOnIsolate=yes"));
	assert!(service.contains("Wants=systemd-udev-trigger.service"));
	assert!(!service.contains("Requires=systemd-udev-trigger.service"));
	assert!(service.contains("Before=sysroot.mount"));

	let script = fs::read_to_string(dir.join("katsu-composefs-live.sh")).unwrap();
	assert!(script.contains("losetup --show -P"), "the payload needs partition scanning");
	assert!(script.contains(r#""${loop}p3""#), "the composefs root is partition 3");
	assert!(script.contains(r#"mount -o ro "${loop}p1" /run/katsu/esp"#));
	assert!(script.contains(r#"mount -o ro "${loop}p2" /run/katsu/xbootldr"#));

	let sysroot = fs::read_to_string(dir.join("sysroot.mount")).unwrap();
	assert!(sysroot.contains("Options=bind"));
	assert!(!sysroot.contains("Type=overlay"));

	let state = fs::read_to_string(dir.join("katsu-composefs-state.service")).unwrap();
	assert!(state.contains("After=sysroot.mount"));
	assert!(state.contains("Before=bootc-root-setup.service"));
}

#[test]
fn ostree_module_overrides_prepare_root() {
	let dir = module("90katsu-ostree");
	let setup = fs::read_to_string(dir.join("module-setup.sh")).unwrap();
	assert!(setup.contains("echo ostree systemd"));
	assert!(setup.contains(r#"rm -f "$initdir/etc/ostree/prepare-root.conf""#));
	assert!(fs::read_to_string(dir.join("prepare-root.conf")).unwrap().contains("enabled=false"));
	assert!(
		fs::read_to_string(dir.join("sysroot.mount"))
			.unwrap()
			.contains("After=katsu-ostree-live.service")
	);
	let service = fs::read_to_string(dir.join("katsu-ostree-live.service")).unwrap();
	assert!(service.contains("IgnoreOnIsolate=yes"));
	assert!(!service.contains("Requires=systemd-udev-trigger.service"));
	let dropin = fs::read_to_string(dir.join("bind-mount.conf")).unwrap();
	assert!(dropin.contains("DefaultDependencies=no"));
	assert!(dropin.contains("IgnoreOnIsolate=yes"));
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
	let script = fs::read_to_string(module("90katsu-ostree").join("katsu-ostree-mountpoints.sh"))
		.unwrap()
		.replace(". /lib/dracut-lib.sh", &format!(". {}", library.display()));
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
