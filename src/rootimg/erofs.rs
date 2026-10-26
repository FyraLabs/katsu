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
	/// --workers=<n>. When unset and dedupe is on, [`DEDUPE_WORKERS`] is used.
	pub workers: Option<u32>,
	/// --compress-hints=<file>
	pub compress_hints: Option<CompressHints>,
}

/// Worker cap applied when global dedupe is on and `workers` is unset.
const DEDUPE_WORKERS: u32 = 2;

/// Where the per-path compression strategy comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressHints {
	/// The built-in [`LIVE_COMPRESS_HINTS`], written next to the target image.
	Live,
	/// A caller-supplied hints file, used directly.
	Path(PathBuf),
}

/// Write the built-in hints into `dir` and return the file's path.
fn write_live_hints(dir: &Path) -> color_eyre::Result<PathBuf> {
	let path = dir.join("erofs-compress-hints.txt");
	std::fs::write(&path, LIVE_COMPRESS_HINTS)?;
	Ok(path)
}

/// 128KiB pclusters for paths a live system reads scattered and small (kernel
/// modules, shared libraries, binaries). Each line is `<pcluster-bytes> <regex>`,
/// matched against in-fs paths with no leading `/`. This equals the current `-C`
/// default, so it only takes effect if `chunk_size` is raised.
pub const LIVE_COMPRESS_HINTS: &str = r"131072 usr/lib/modules/.*\.ko.*
131072 usr/lib64/.*\.so.*
131072 usr/lib/.*\.so.*
131072 usr/bin/.*
";
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
		// `Live` is written next to the target by `erofs_mkfs`.
		if let Some(CompressHints::Path(path)) = &self.compress_hints {
			args.push(format!("--compress-hints={}", path.display()));
		}
		if !self.extra_features.is_empty() {
			let features = self.extra_features.join(",");
			args.push(format!("-E{features}"));
		}

		// Cap workers under dedupe unless the caller chose a count.
		if let Some(workers) = self.workers.or(self.dedupe_enabled().then_some(DEDUPE_WORKERS)) {
			args.push(format!("--workers={workers}"));
		}

		if self.tar_mode {
			args.push("--tar=f".to_string());
		}
		args
	}

	/// Whether global full-file deduplication is on.
	pub fn dedupe_enabled(&self) -> bool {
		self.extra_features.iter().any(|f| f == "dedupe")
	}

	/// Turn global deduplication on or off.
	pub fn set_dedupe(&mut self, enabled: bool) {
		self.extra_features.retain(|f| f != "dedupe");
		if enabled {
			self.extra_features.push("dedupe".to_string());
		}
	}
}

impl Default for MkfsErofsOptions {
	fn default() -> Self {
		MkfsErofsOptions {
			tar_mode: false,
			compression: Some("zstd,level=6".into()),
			// 128k pcluster
			chunk_size: Some(131072),
			xattr_level: Some(1),
			exclude_paths: ["/sys/", "/proc/"].iter().map(|s| s.to_string()).collect(),
			file_contexts: None,
			log_level: 0,
			workers: None,
			// only dedupe inode, seems to be good default balance
			extra_features: ["fragments", "fragdedupe=inode"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
			// `erofs-compress-hints=none` opts out.
			compress_hints: Some(CompressHints::Live),
		}
	}
}

pub fn erofs_mkfs(
	source: &Path, target: &Path, options: &MkfsErofsOptions,
) -> color_eyre::Result<PathBuf> {
	let mut cmd = std::process::Command::new("mkfs.erofs");
	let mut args = options.build_args();

	if let Some(CompressHints::Live) = options.compress_hints {
		let hints = write_live_hints(target.parent().unwrap_or_else(|| Path::new(".")))?;
		args.push(format!("--compress-hints={}", hints.display()));
	}

	cmd.args(&args);
	cmd.arg(target);
	cmd.arg(source);

	tracing::info!("Creating EROFS image: {:?}", cmd);
	let output =
		cmd.status().map_err(|e| crate::util::tool_error("mkfs.erofs", "erofs-utils", e))?;
	if !output.success() {
		return Err(color_eyre::eyre::eyre!(
			"mkfs.erofs failed with exit code: {}",
			output.code().unwrap_or(-1)
		));
	}
	Ok(target.to_path_buf())
}
