//! Wrapper for `mkfs.erofs` command line utility.

use std::path::{Path, PathBuf};

pub struct MkfsErofsOptions {
	/// -z<compression>
	pub compression: Option<String>,
	/// -C<chunk_size>
	pub chunk_size: Option<u32>,

	/// -x<xattr_level>
	pub xattr_level: Option<u32>,

	/// --exclude-path=<path> (repeatable)
	pub exclude_paths: Vec<String>,
	// selinux contexts
	pub file_contexts: Option<String>,
	/// log level
	// #[default = "0"]
	pub log_level: u32,
	pub extra_features: Vec<String>,
	pub tar_mode: bool,
}

impl MkfsErofsOptions {
	pub fn build_args(&self) -> Vec<String> {
		let mut args = Vec::new();

		args.push(format!("-d{}", self.log_level));
		if self.log_level == 0 {
			args.push("--quiet".to_string());
		}
		if let Some(ref compression) = self.compression {
			args.push(format!("-z{compression}"));
		}
		if let Some(xattr_level) = self.xattr_level {
			args.push(format!("-x{xattr_level}"));
		}
		if let Some(chunk_size) = self.chunk_size {
			args.push(format!("-C{chunk_size}"));
		}
		for path in &self.exclude_paths {
			args.push(format!("--exclude-path={}", path));
		}
		if let Some(ref contexts) = self.file_contexts {
			args.push(format!("--file-contexts={}", contexts));
		}
		if !self.extra_features.is_empty() {
			let features = self.extra_features.join(",");
			args.push(format!("-E{features}"));
		}

		if self.tar_mode {
			args.push("--tar=f".to_string());
		}
		args
	}
}

impl Default for MkfsErofsOptions {
	fn default() -> Self {
		MkfsErofsOptions {
			tar_mode: false,
			compression: Some("zstd,level=5".into()),
			chunk_size: Some(1048576),
			xattr_level: Some(1),
			exclude_paths: ["/sys/", "/proc/"].iter().map(|s| s.to_string()).collect(),
			file_contexts: None,
			log_level: 0,
			extra_features: ["all-fragments", "fragdedupe=inode"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		}
	}
}

pub fn erofs_mkfs(
	source: &Path, target: &Path, options: &MkfsErofsOptions,
) -> color_eyre::Result<PathBuf> {
	let mut cmd = std::process::Command::new("mkfs.erofs");
	let args = options.build_args();
	cmd.args(&args);
	cmd.arg(target);
	cmd.arg(source);

	tracing::info!("Creating EROFS image: {:?}", cmd);
	let output = cmd.status()?;
	if !output.success() {
		return Err(color_eyre::eyre::eyre!(
			"mkfs.erofs failed with exit code: {}",
			output.code().unwrap_or(-1)
		));
	}
	Ok(target.to_path_buf())
}
