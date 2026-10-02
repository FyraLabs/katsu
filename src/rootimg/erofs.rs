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
	/// --workers=<n>: number of worker threads. Defaults to the CPU count when
	/// unset, matching mkfs.erofs' own default.
	pub workers: Option<u32>,
	/// Default number of worker threads when dedup is enabled.
	pub(crate) dedupe_workers: u32,
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

		// Dedup memory use scales with the number of workers, and it is the setting
		// that previously exhausted RAM. Cap it unless the caller asked explicitly.
		if self.dedupe_enabled() && self.workers.is_none() {
			args.push(format!("--workers={}", self.dedupe_workers));
		}

		if let Some(workers) = self.workers {
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
}

impl Default for MkfsErofsOptions {
	fn default() -> Self {
		MkfsErofsOptions {
			tar_mode: false,
			compression: Some("zstd,level=6".into()),
			chunk_size: Some(1048576),
			xattr_level: Some(1),
			exclude_paths: ["/sys/", "/proc/"].iter().map(|s| s.to_string()).collect(),
			file_contexts: None,
			log_level: 0,
			workers: None,
			// Dedup is on by default: it is what keeps repeated content from being
			// stored twice, and the ISO is the artifact users actually download.
			// `--workers` is capped for it because memory scales with worker count;
			// an uncapped run previously exhausted RAM on a ~10G tree. Set
			// `KATSU_EROFS_WORKERS` to raise it deliberately, or
			// `KATSU_EROFS_DEDUPE=0` to disable dedup for a faster build.
			dedupe_workers: 2,
			// `fragdedupe=full` always dedupes fragments by content, whereas `inode`
			// only dedupes when inode data is identical (faster, less effective).
			// Size is the priority for shipped media, so pay the build cost.
			extra_features: ["all-fragments", "fragdedupe=full", "dedupe"]
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
	let output = cmd.status().map_err(|e| {
		if e.kind() == std::io::ErrorKind::NotFound {
			color_eyre::eyre::eyre!(
				"mkfs.erofs not found; install erofs-utils (e.g. `dnf install erofs-utils`)"
			)
		} else {
			color_eyre::eyre::eyre!("Running mkfs.erofs: {e}")
		}
	})?;
	if !output.success() {
		return Err(color_eyre::eyre::eyre!(
			"mkfs.erofs failed with exit code: {}",
			output.code().unwrap_or(-1)
		));
	}
	Ok(target.to_path_buf())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn features(args: &[String]) -> String {
		args.iter().find(|a| a.starts_with("-E")).expect("expected an -E feature argument").clone()
	}

	#[test]
	fn dedupe_is_enabled_by_default_with_a_capped_worker_count() {
		// Dedup is expensive and its memory use scales with workers, so the
		// default must bound parallelism instead of letting mkfs.erofs pick.
		let opts = MkfsErofsOptions::default();
		assert!(opts.dedupe_enabled());
		let args = opts.build_args();
		assert!(features(&args).contains("dedupe"));
		assert!(features(&args).contains("fragdedupe=full"));
		let expected = format!("--workers={}", opts.dedupe_workers);
		assert!(args.contains(&expected));
	}

	#[test]
	fn explicit_workers_overrides_the_dedupe_cap() {
		// A caller that picked a worker count must not be silently second-guessed.
		let opts = MkfsErofsOptions { workers: Some(7), ..Default::default() };
		let args = opts.build_args();
		assert!(args.contains(&"--workers=7".to_string()));
		assert_eq!(args.iter().filter(|a| a.starts_with("--workers")).count(), 1);
	}

	#[test]
	fn dedupe_can_be_disabled() {
		let mut opts = MkfsErofsOptions::default();
		opts.extra_features.retain(|f| f != "dedupe");
		assert!(!opts.dedupe_enabled());
		let args = opts.build_args();
		assert!(!args.iter().any(|a| a.starts_with("--workers")));
	}
}
