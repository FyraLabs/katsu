#[macro_export]
macro_rules! run {
	($n:expr $(, $arr:expr)* $(,)?) => {{
		let out = std::process::Command::new($n)
		.args([$($arr,)*])
		.output()?;
		if out.status.success() {
			Ok(out.stdout)
		} else {
			use color_eyre::{eyre::eyre, SectionExt, Help};
			let stdout = String::from_utf8_lossy(&out.stdout);
			let stderr = String::from_utf8_lossy(&out.stderr);
			Err(eyre!("Command returned non-zero code"))
				.with_section(move || stdout.trim().to_string().header("Stdout:"))
				.with_section(move || stderr.trim().to_string().header("Stdout:"))
		}
	}};
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
