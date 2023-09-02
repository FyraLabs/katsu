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
	tracing::info!("Executing command");
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
		Ok(Self::from(&*cmd_lib::run_fun!(uname - m)?))
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
