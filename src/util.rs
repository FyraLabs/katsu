use color_eyre::Result;
use std::path::Path;
use tracing::{debug, error};

#[macro_export]
macro_rules! run {
	($n:expr $(, $arr:expr)* $(,)?) => {{
		run!($n; [$($arr,)*])
	}};
	($n:expr; $arr:expr) => {{
		$crate::util::exec($n, &$arr.to_vec(), true)
	}};
	(~$n:expr $(, $arr:expr)* $(,)?) => {{
		run!(~$n; [$($arr,)*])
	}};
	(~$n:expr; $arr:expr) => {{
		$crate::util::exec($n, &$arr.to_vec(), false)
	}};
}

/// Macro that wraps around cmd_lib::run_cmd!, but runs it in a chroot
///
/// First argument is the chroot path, the following arguments are the command and arguments
///
/// Example:
/// ```rs
/// chroot_run!(PathBuf::from("/path/to/chroot"), "dnf", "install", "-y", "vim");
/// ```
///
/// Uses run! but `unshare -R` prepended with first argument
#[macro_export]
macro_rules! chroot_run {
	($chroot:expr, $n:expr $(, $arr:expr)* $(,)?) => {{
		chroot_run!($chroot, $n; [$($arr,)*])
	}};
	($chroot:expr, $n:expr; $arr:expr) => {{
		$crate::util::run_with_chroot(&std::path::PathBuf::from($chroot), || {
			$crate::run!($n; $arr)?;
			Ok(())
		})
	}};
	(~$chroot:expr, $n:expr $(, $arr:expr)* $(,)?) => {{
		chroot_run!(~$chroot, $n; [$($arr,)*])
	}};
	(~$chroot:expr, $n:expr; $arr:expr) => {{
		$crate::util::run_with_chroot(&std::path::PathBuf::from($chroot), || {
			$crate::run!(~$n; $arr)?;
			Ok(())
		})
	}};
}

/// Wraps around cmd_lib::run_cmd!, but mounts the chroot
/// Example:
///
/// ```rs
/// chroot_run!(PathBuf::from("/path/to/chroot"), chroot /path/to/chroot echo "hello world" > /hello.txt);
/// ```
#[macro_export]
macro_rules! chroot_run_cmd {
	($chroot:expr, $($cmd:tt)*) => {{
		$crate::util::run_with_chroot(&PathBuf::from($chroot), || {
			tracing::debug!("Running command: {}", stringify!($($cmd)*) );
			cmd_lib::run_cmd!($($cmd)*)?;
			Ok(())
		})
	}};
}

/// Runs in chroot, returns stdout
#[macro_export]
macro_rules! chroot_run_fun {
	($chroot:expr, $($cmd:tt)*) => {{
		$crate::util::run_with_chroot(&PathBuf::from($chroot), || {
			cmd_lib::run_fun!($($cmd)*)?;
			Ok(())
		})
	}};
}

/// Perform the let statement, else bail out with specified error message
#[macro_export]
macro_rules! bail_let {
	($left:pat = $right:expr => $err:expr) => {
		#[rustfmt::skip]
		let $left = $right else {
			return Err(color_eyre::eyre::eyre!($err));
		};
	};
}

/// Automatically generates prepend comments
#[macro_export]
macro_rules! prepend_comment {
	($var:ident: $path:literal, $desc:literal, $module:path) => {
		const $var: &str = concat!(
			"#\n# ",
			$path,
			": ",
			$desc,
			"\n# Automatically generated by Katsu Image Builder. See\n# ",
			stringify!($module),
			" for more information.\n\n"
		);
	};
}

/// Generates the file content using the template given
#[macro_export]
macro_rules! tpl {
	(@match $name:ident) => {
		$name
	};
	(@match $name:ident: $var:expr) => {
		($var)
	};
	($tmpl:expr => {$($name:ident$(: $var:expr)?),*} $(=>$out:expr)?) => {{
		tracing::debug!(tmpl=?$tmpl, "Generating file from template");
		let mut tera = tera::Tera::default();
		let mut ctx = tera::Context::new();
		$(
			ctx.insert(stringify!($name), &$crate::tpl!(@match $name$(: $var)?));
		)*
		let out = tera.render_str(include_str!(concat!("../templates/", $tmpl)), &ctx)?;
		tracing::trace!(out, path = $tmpl, "tpl!() Template output");
		$(
			tracing::debug!(tmpl=?$tmpl, outfile=?$out, "Writing template output to file");
			$crate::util::just_write($out, &out)?;
		)?
		out
	}};
}

#[macro_export]
macro_rules! gen_phase {
	($skip_phases: ident) => {
		macro_rules! phase {
			($key:literal: $run:expr) => {
				if !$skip_phases.contains($key) {
					tracing::info_span!(concat!("phase$", $key)).in_scope(
						|| -> color_eyre::Result<()> {
							tracing::info!("Starting phase `{}`", $key);
							$run?;
							tracing::info!("Finished phase `{}`", $key);
							Ok(())
						},
					)?;
				} else {
					tracing::info!("Skipping phase `{}`", $key);
				}
			};
		}
	};
}

#[tracing::instrument]
pub fn exec(cmd: &str, args: &[&str], pipe: bool) -> color_eyre::Result<Vec<u8>> {
	tracing::debug!("Executing command");
	let out = std::process::Command::new(cmd)
		.args(args)
		.stdout(if pipe { std::process::Stdio::piped() } else { std::process::Stdio::inherit() })
		.stderr(if pipe { std::process::Stdio::piped() } else { std::process::Stdio::inherit() })
		.output()?;
	if out.status.success() {
		return if pipe {
			let stdout = String::from_utf8_lossy(&out.stdout);
			let stderr = String::from_utf8_lossy(&out.stderr);
			tracing::trace!(?stdout, ?stderr, "Command succeeded");
			Ok(out.stdout)
		} else {
			tracing::trace!("Command succeeded");
			Ok(vec![])
		};
	}
	use color_eyre::{eyre::eyre, Help, SectionExt};
	if pipe {
		let stdout = String::from_utf8_lossy(&out.stdout);
		let stderr = String::from_utf8_lossy(&out.stderr);
		Err(eyre!("Command returned code: {}", out.status.code().unwrap_or_default()))
			.with_section(move || stdout.trim().to_string().header("Stdout:"))
			.with_section(move || stderr.trim().to_string().header("Stderr:"))
	} else {
		Err(eyre!("Command returned code: {}", out.status.code().unwrap_or_default()))
	}
}

// ? https://stackoverflow.com/questions/45125516/possible-values-for-uname-m
#[derive(Default)]
pub enum Arch {
	X86,
	X86_64,
	ArmV7l,  // armv7l
	AArch64, // aarch64
	#[default]
	Nyani, // にゃんに？？ｗ
}

impl Arch {
	pub fn get() -> color_eyre::Result<Self> {
		Ok(Self::from(&*cmd_lib::run_fun!(uname -m;)?))
	}
}

impl From<&str> for Arch {
	fn from(value: &str) -> Self {
		match value {
			"i386" => Self::X86,
			"x86_64" => Self::X86_64,
			"armv7l" => Self::ArmV7l,
			"aarch64" => Self::AArch64,
			_ => Self::Nyani,
		}
	}
}

impl From<Arch> for &str {
	fn from(value: Arch) -> &'static str {
		match value {
			Arch::X86 => "i386",
			Arch::X86_64 => "x86_64",
			Arch::ArmV7l => "armv7l",
			Arch::AArch64 => "aarch64",
			_ => panic!("Unknown architecture"),
		}
	}
}

const MNTS: &[(&str, &str, Option<&str>, nix::mount::MsFlags); 4] = &[
	("/proc", "proc", Some("proc"), nix::mount::MsFlags::empty()),
	("/sys", "sys", Some("sysfs"), nix::mount::MsFlags::empty()),
	("/dev", "dev", None, nix::mount::MsFlags::MS_BIND),
	("/dev/pts", "dev/pts", None, nix::mount::MsFlags::MS_BIND),
];

/// Prepare chroot by mounting /dev, /proc, /sys
pub fn prepare_chroot(root: &Path) -> Result<()> {
	debug!("Preparing chroot");

	// cmd_lib::run_cmd! (
	// 	mkdir -p $root/proc;
	// 	mount -t proc proc $root/proc;
	// 	mkdir -p $root/sys;
	// 	mount -t sysfs sys $root/sys;
	// 	mkdir -p $root/dev;
	// 	mount -o bind /dev $root/dev;
	// 	mkdir -p $root/dev/pts;
	// 	mount -o bind /dev $root/dev/pts;
	// 	sh -c "mv $root/etc/resolv.conf $root/etc/resolv.conf.bak || true";
	// 	cp /etc/resolv.conf $root/etc/resolv.conf;
	// )?;
	// rewrite the above with

	for (src, target, fstype, flags) in MNTS {
		let target = root.join(target);
		std::fs::create_dir_all(&target)?;
		debug!("Mounting {src:?} to {target:?}");
		let mut i = 0;
		loop {
			if nix::mount::mount(Some(*src), &target, *fstype, *flags, None::<&str>).is_ok() {
				break;
			}
			i += 1;
			error!("Failed to mount {target:?}, {i} tries out of 10");
			// wait 500ms
			std::thread::sleep(std::time::Duration::from_millis(500));
			if i > 10 {
				break;
			}
		}
	}

	std::fs::create_dir_all(root.join("etc"))?;
	std::fs::copy("/etc/resolv.conf", root.join("etc/resolv.conf"))?;

	Ok(())
}

/// Unmount /dev, /proc, /sys
pub fn unmount_chroot(root: &Path) -> Result<()> {
	debug!("Unmounting chroot");
	// cmd_lib::run_cmd! (
	// 	umount $root/dev/pts;
	// 	umount $root/dev;
	// 	umount $root/sys;
	// 	umount $root/proc;
	// 	sh -c "mv $root/etc/resolv.conf.bak $root/etc/resolv.conf || true";
	// )?;
	// loop until all unmounts are successful

	let mounts = vec![root.join("dev/pts"), root.join("dev"), root.join("sys"), root.join("proc")];

	for mount in mounts {
		let mut i = 0;

		loop {
			// combine mntflags: MNT_FORCE | MNT_DETACH
			if nix::mount::umount2(
				&mount.canonicalize()?,
				nix::mount::MntFlags::MNT_FORCE.union(nix::mount::MntFlags::MNT_DETACH),
			)
			.is_ok()
			{
				break;
			}
			i += 1;
			error!("Failed to unmount {mount:?}, {i} tries out of 10");
			// wait 500ms
			std::thread::sleep(std::time::Duration::from_millis(500));
			if i > 10 {
				break;
			}
		}
	}

	// nix::mount::umount2(&root.join("dev/pts"), nix::mount::MntFlags::MNT_FORCE)?;
	// let umount = nix::mount::umount2(&root.join("dev"), nix::mount::MntFlags::MNT_FORCE);

	// nix::mount::umount2(&root.join("sys"), nix::mount::MntFlags::MNT_FORCE)?;
	// nix::mount::umount2(&root.join("proc"), nix::mount::MntFlags::MNT_FORCE)?;
	Ok(())
}
/// Mount chroot devices, then run function
///
/// NOTE: This function requires that the function inside returns a result, so we can catch errors and unmount early
pub fn run_with_chroot<T>(root: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
	prepare_chroot(root)?;
	let res = f();
	unmount_chroot(root)?;
	res
}

/// Create an empty sparse file with given size
pub fn create_sparse(path: &Path, pos: u64) -> Result<std::fs::File> {
	use std::io::{Seek, Write};
	debug!(?path, pos, "Creating sparse file");
	let mut f = std::fs::File::create(path)?;
	f.seek(std::io::SeekFrom::Start(pos))?;
	f.write_all(&[0])?;
	Ok(f)
}

pub struct LoopDevHdl(loopdev::LoopDevice);

impl Drop for LoopDevHdl {
	fn drop(&mut self) {
		let Err(e) = self.0.detach() else { return };
		tracing::warn!("Fail to detach loopdev: {e:#}");
	}
}

pub fn loopdev_with_file(path: &Path) -> Result<(std::path::PathBuf, LoopDevHdl)> {
	let lc = loopdev::LoopControl::open()?;
	let loopdev = lc.next_free()?;
	loopdev.attach_file(path)?;
	crate::bail_let!(Some(ldp) = loopdev.path() => "Fail to unwrap loopdev.path() = None");
	Ok((ldp, LoopDevHdl(loopdev)))
}

pub fn just_write(path: impl AsRef<Path>, content: impl AsRef<str>) -> Result<()> {
	use std::io::Write;
	let (path, content) = (path.as_ref(), content.as_ref());
	tracing::trace!(?path, content, "Writing content to file");
	crate::bail_let!(Some(parent) = path.parent() => "Invalid file path");
	let _ = std::fs::create_dir_all(parent);
	std::fs::File::create(path)?.write_all(content.as_bytes())?;
	Ok(())
}
