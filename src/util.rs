#[macro_export]
macro_rules! run {
	($n:expr $(, $arr:expr)* $(,)?) => {{
		run!($n; [$($arr,)*])
	}};
	($n:expr; $arr:expr) => {{
		crate::util::exec($n, &$arr.to_vec())
	}}
}

#[tracing::instrument]
pub fn exec(cmd: &str, args: &[&str]) -> color_eyre::Result<Vec<u8>> {
	tracing::debug!("Executing command");
	let out = std::process::Command::new(cmd).args(args).output()?;
	if out.status.success() {
		let stdout = String::from_utf8_lossy(&out.stdout);
		let stderr = String::from_utf8_lossy(&out.stderr);
		tracing::debug!(?stdout, ?stderr, "Command succeeded");
		Ok(out.stdout)
	} else {
		use color_eyre::{eyre::eyre, Help, SectionExt};
		let stdout = String::from_utf8_lossy(&out.stdout);
		let stderr = String::from_utf8_lossy(&out.stderr);
		Err(eyre!("Command returned code: {}", out.status.code().unwrap_or_default()))
			.with_section(move || stdout.trim().to_string().header("Stdout:"))
			.with_section(move || stderr.trim().to_string().header("Stderr:"))
	}
}

#[derive(Default)]
pub enum Arch {
	X86,
	X86_64,
	#[default]
	Nyani, // にゃんに？？ｗ
}

impl From<&str> for Arch {
	fn from(value: &str) -> Self {
		match value {
			"i386" => Self::X86,
			"x86_64" => Self::X86_64,
			_ => Self::Nyani,
		}
	}
}

impl Into<&str> for Arch {
	fn into(self) -> &'static str {
		match self {
			Self::X86 => "i386",
			Self::X86_64 => "x86_64",
			_ => panic!("Unknown architecture"),
		}
	}
}
