use std::path::PathBuf;

use color_eyre::Result;
use tracing::debug;

#[macro_export]
macro_rules! run {
	($n:expr $(, $arr:expr)* $(,)?) => {{
		run!($n; [$($arr,)*])
	}};
	($n:expr; $arr:expr) => {{
		crate::util::exec($n, &$arr.to_vec(), true)
	}};
	(~$n:expr $(, $arr:expr)* $(,)?) => {{
		run!(~$n; [$($arr,)*])
	}};
	(~$n:expr; $arr:expr) => {{
		crate::util::exec($n, &$arr.to_vec(), false)
	}};
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
	ArmV7l, // armv7l
	AArch64, // aarch64
	#[default]
	Nyani, // にゃんに？？ｗ
}

impl Arch {
	pub fn get() -> color_eyre::Result<Self> {
		Ok(Self::from(&*cmd_lib::run_fun!(uname -m)?))
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

impl Into<&str> for Arch {
	fn into(self) -> &'static str {
		match self {
			Self::X86 => "i386",
			Self::X86_64 => "x86_64",
			Self::ArmV7l => "armv7l",
			Self::AArch64 => "aarch64",
			_ => panic!("Unknown architecture"),
		}
	}
}


/// Prepare chroot by mounting /dev, /proc, /sys
pub fn prepare_chroot(root: PathBuf) -> Result<()> {
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

	std::fs::create_dir_all(root.clone())?;

	let proc_pbuf = root.join("proc");

	std::fs::create_dir_all(&proc_pbuf)?;

	nix::mount::mount(
		Some("/proc"),
		&root.join("proc"),
		Some("proc"),
		nix::mount::MsFlags::empty(),
		None::<&str>,
	)?;

	let sys_pbuf = root.join("sys");

	std::fs::create_dir_all(&sys_pbuf)?;

	nix::mount::mount(
		Some("/sys"),
		&root.join("sys"),
		Some("sysfs"),
		nix::mount::MsFlags::empty(),
		None::<&str>,
	)?;

	let dev_pbuf = root.join("dev");

	std::fs::create_dir_all(&dev_pbuf.join("pts"))?;

	// bind mount this one instead

	nix::mount::mount(
		Some("/dev"),
		&root.join("dev"),
		None::<&str>,
		nix::mount::MsFlags::MS_BIND,
		None::<&str>,
	)?;

	nix::mount::mount(
		Some("/dev/pts"),
		&root.join("dev/pts"),
		None::<&str>,
		nix::mount::MsFlags::MS_BIND,
		None::<&str>,
	)?;

	// copy resolv.conf

	let resolv_conf = std::fs::read_to_string("/etc/resolv.conf")?;

	std::fs::write(root.join("etc/resolv.conf"), resolv_conf)?;

	Ok(())
}

/// Unmount /dev, /proc, /sys
pub fn unmount_chroot(root: PathBuf) -> Result<()> {
	debug!("Unmounting chroot");
	// cmd_lib::run_cmd! (
	// 	umount $root/dev/pts;
	// 	umount $root/dev;
	// 	umount $root/sys;
	// 	umount $root/proc;
	// 	sh -c "mv $root/etc/resolv.conf.bak $root/etc/resolv.conf || true";
	// )?;
	nix::mount::umount(&root.join("dev/pts"))?;
	nix::mount::umount(&root.join("dev"))?;
	nix::mount::umount(&root.join("sys"))?;
	nix::mount::umount(&root.join("proc"))?;
	Ok(())
}
/// Mount chroot devices, then run function
pub fn run_with_chroot<T>(root: &PathBuf, f: impl FnOnce() -> T) -> Result<T> {
	prepare_chroot(root.to_path_buf())?;
	let res = f();
	unmount_chroot(root.to_path_buf())?;
	Ok(res)
}



