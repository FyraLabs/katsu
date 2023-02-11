#[macro_export]
macro_rules! run {
	($n:expr $(, $arr:expr)* $(,)?) => {{
		run!($n; [$($arr,)*])
	}};
	($n:expr; $arr:expr) => {{
		tracing::debug!("# {} {:?}", $n, $arr);
		let out = std::process::Command::new($n)
		.args($arr)
		.output()?;
		if out.status.success() {
			Ok(out.stdout)
		} else {
			use color_eyre::{eyre::eyre, SectionExt, Help};
			let stdout = String::from_utf8_lossy(&out.stdout);
			let stderr = String::from_utf8_lossy(&out.stderr);
			Err(eyre!("Command returned code: {}", out.status.code().unwrap_or_default()))
				.with_section(move || stdout.trim().to_string().header("Stdout:"))
				.with_section(move || stderr.trim().to_string().header("Stdout:"))
		}
	}}
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
