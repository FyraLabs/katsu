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
	/// --compress-hints=<file>: a per-path compression strategy. Each line is
	/// `<pcluster-size> [algorithm-index] <regex>`, matched against paths inside
	/// the output filesystem with no leading `/`.
	pub compress_hints: Option<CompressHints>,
}

/// Where the per-path compression strategy comes from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompressHints {
	/// The built-in strategy for live media. Needs a directory to materialise the
	/// hints file in, which [`erofs_mkfs`] supplies from the target's location.
	#[default]
	Live,
	/// A caller-supplied hints file, used directly.
	Path(PathBuf),
}

impl CompressHints {
	/// Resolve to a hints file on disk, writing the built-in one when needed.
	///
	/// `scratch_dir` is where the built-in strategy is written; it comes from the
	/// image's own directory so a build never writes outside its workspace.
	pub(crate) fn resolve(&self, scratch_dir: &Path) -> color_eyre::Result<PathBuf> {
		match self {
			Self::Path(path) => Ok(path.clone()),
			Self::Live => {
				let path = scratch_dir.join("erofs-compress-hints.txt");
				std::fs::write(&path, LIVE_COMPRESS_HINTS)?;
				Ok(path)
			},
		}
	}
}

/// The built-in `live` strategy, as text.
///
/// Each line is `<pcluster-size-bytes> <regex>`. Patterns match paths *inside*
/// the output filesystem, so they have no leading `/`.
///
/// A live system reads many small, scattered files from large binaries: kernel
/// modules during device bring-up, shared libraries at every `exec`. With a 1MiB
/// physical cluster, touching one 4KiB page can decompress the whole cluster, so
/// those paths get 128KiB clusters while bulk data keeps the larger extents that
/// compress better.
pub const LIVE_COMPRESS_HINTS: &str = "131072 usr/lib/modules/.*\\.ko.*\n\
131072 usr/lib64/.*\\.so.*\n\
131072 usr/lib/.*\\.so.*\n\
131072 usr/bin/.*\n";

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
		// The `Live` variant writes its file next to the image; `erofs_mkfs`
		// resolves it, since only it knows the target path.
		if let Some(CompressHints::Path(path)) = &self.compress_hints {
			args.push(format!("--compress-hints={}", path.display()));
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
			// 128KiB physical clusters: a 1MiB cluster means touching one 4KiB page can
			// decompress the whole cluster, which shows up as slow boot and read times
			// on live media. Smaller clusters cost some size, so hot paths get them via
			// `compress_hints` while bulk data keeps larger extents.
			chunk_size: Some(131072),
			xattr_level: Some(1),
			exclude_paths: ["/sys/", "/proc/"].iter().map(|s| s.to_string()).collect(),
			file_contexts: None,
			log_level: 0,
			workers: None,
			// Only relevant when global dedup is asked for; it bounds the compression
			// workers, not the dedup index.
			dedupe_workers: 2,
			// `fragments` (not `all-fragments`) packs fragment data into the shared
			// inode without force-packing every whole file. `all-fragments` compresses
			// marginally better but makes reads pull more data per file, which is the
			// tradeoff working against live boot read latency.
			//
			// `fragdedupe=inode` dedupes only when inode data is identical. That is the
			// measured-safe setting: `fragdedupe=full` compares every fragment's content
			// and OOM-killed the build host (RAM *and* swap exhausted) on an ~18G tree,
			// where the `inode` form completed and still deduplicated ~17.8G.
			//
			// Global `dedupe` is deliberately *not* in the default set. Measured on the
			// Ultramarine Plasma payload at `-C131072`, adding it changed the EROFS
			// phase from ~10min to ~35min and `mkfs.erofs` peak RSS from 0.6G to 8.3G
			// while the payload was, if anything, slightly larger (4.67G -> 4.84G). It
			// is single-threaded by design (mkfs.erofs warns "multi-threaded dedupe is
			// NOT implemented"), so `--workers` does not bound its memory: it is an
			// index over the whole tree, and the 128KiB clusters used here multiply the
			// extent count it has to track. `fragdedupe=inode` already catches identical
			// files, which is where the size win actually comes from.
			// `KATSU_FEATURE_FLAGS=erofs-dedupe` opts back in.
			extra_features: ["fragments", "fragdedupe=inode"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
			// Per-path cluster sizes for the paths a live system reads scattered and
			// small. `erofs-compress-hints=none` opts out.
			compress_hints: Some(CompressHints::Live),
		}
	}
}

pub fn erofs_mkfs(
	source: &Path, target: &Path, options: &MkfsErofsOptions,
) -> color_eyre::Result<PathBuf> {
	let mut cmd = std::process::Command::new("mkfs.erofs");
	let mut args = options.build_args();

	// A built-in hints strategy needs a file on disk; the target's directory is
	// the one place we know is writable and scoped to this build.
	if let Some(CompressHints::Live) = options.compress_hints {
		let scratch_dir = target.parent().unwrap_or_else(|| Path::new("."));
		let hints = CompressHints::Live.resolve(scratch_dir)?;
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

#[cfg(test)]
mod tests {
	use super::*;

	fn features(args: &[String]) -> String {
		args.iter().find(|a| a.starts_with("-E")).expect("expected an -E feature argument").clone()
	}

	/// The individual `-E` features, so a lookup for `dedupe` does not match the
	/// `fragdedupe=inode` entry that merely contains the substring.
	fn feature_list(args: &[String]) -> Vec<String> {
		features(args).trim_start_matches("-E").split(',').map(str::to_string).collect()
	}

	#[test]
	fn dedupe_is_off_by_default() {
		// Global dedup is single-threaded and index-based: measured on the Plasma
		// payload at `-C131072` it took the EROFS phase from ~10min to ~35min and
		// `mkfs.erofs` RSS from 0.6G to 8.3G, with the payload marginally larger.
		// `fragdedupe=inode` is what actually dedupes the repeated files.
		let opts = MkfsErofsOptions::default();
		assert!(!opts.dedupe_enabled());
		let args = opts.build_args();
		assert!(!feature_list(&args).iter().any(|f| f == "dedupe"));
		// `--workers` only bounds dedup, so it must not be emitted without it.
		assert!(!args.iter().any(|a| a.starts_with("--workers")));
		// The fragment dedup that carries the size win stays on.
		assert!(features(&args).contains("fragdedupe=inode"));
	}

	#[test]
	fn dedupe_can_be_opted_into_with_a_capped_worker_count() {
		// Opting in must still bound the compression workers, since dedup's own
		// index is single-threaded and unaffected by the flag.
		let mut opts = MkfsErofsOptions::default();
		opts.set_dedupe(true);
		assert!(opts.dedupe_enabled());
		let args = opts.build_args();
		assert!(feature_list(&args).iter().any(|f| f == "dedupe"));
		assert!(args.contains(&format!("--workers={}", opts.dedupe_workers)));
	}

	#[test]
	fn set_dedupe_toggles_without_duplicating_the_feature() {
		let mut opts = MkfsErofsOptions::default();
		opts.set_dedupe(true);
		opts.set_dedupe(true);
		assert_eq!(opts.extra_features.iter().filter(|f| *f == "dedupe").count(), 1);
		opts.set_dedupe(false);
		assert!(!opts.dedupe_enabled());
	}

	#[test]
	fn defaults_favor_read_latency_over_size() {
		// Live media is read scattered and small, so the shipped defaults use the
		// milder fragments mode and 128KiB clusters instead of the size-maximising
		// `all-fragments` with 1MiB clusters.
		let opts = MkfsErofsOptions::default();
		assert_eq!(opts.chunk_size, Some(131072));
		let feats = features(&opts.build_args());
		assert!(feats.contains("fragments"), "expected `fragments`: {feats}");
		assert!(
			!feats.contains("all-fragments"),
			"`all-fragments` trades read latency for size: {feats}"
		);
		assert_eq!(opts.compress_hints, Some(CompressHints::Live));
	}

	#[test]
	fn compress_hints_are_passed_through_when_set() {
		// An explicit hints file must reach mkfs.erofs verbatim rather than being
		// dropped. The `Live` variant is resolved by `erofs_mkfs`, which is the only
		// place that knows the target's directory.
		let opts = MkfsErofsOptions {
			compress_hints: Some(CompressHints::Path(PathBuf::from("/tmp/hints.txt"))),
			..Default::default()
		};
		let args = opts.build_args();
		assert!(args.contains(&"--compress-hints=/tmp/hints.txt".to_string()));
	}

	#[test]
	fn compress_hints_are_absent_by_default() {
		let opts = MkfsErofsOptions { compress_hints: None, ..Default::default() };
		assert!(!opts.build_args().iter().any(|a| a.starts_with("--compress-hints")));
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
	fn live_compress_hints_match_in_fs_paths_without_a_leading_slash() {
		// `--compress-hints` matches against paths *inside* the output filesystem with
		// no leading `/`. A leading slash, or a missing escape, would silently match
		// nothing and the flag would look like it worked while doing nothing.
		let dir = std::env::temp_dir().join(format!("katsu-hints-{}", uuid::Uuid::new_v4()));
		std::fs::create_dir_all(&dir).unwrap();
		let path = CompressHints::Live.resolve(&dir).unwrap();
		let contents = std::fs::read_to_string(&path).unwrap();

		let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
		assert!(!lines.is_empty(), "the built-in strategy must not be empty");
		for line in lines {
			let mut fields = line.split_whitespace();
			let size: u64 = fields
				.next()
				.expect("a pcluster size")
				.parse()
				.expect("the pcluster size must be numeric");
			// mkfs.erofs requires the cluster size to be a whole number of sectors.
			assert_eq!(size % 512, 0, "{size} is not sector aligned in {line:?}");
			let pattern = fields.next().expect("a match pattern");
			assert!(
				!pattern.starts_with('/'),
				"patterns match in-fs paths and must not start with `/`: {pattern:?}"
			);
		}
		// The kernel-module pattern has to survive escaping to match `foo.ko.xz`.
		assert!(
			contents.contains(r"usr/lib/modules/.*\.ko.*"),
			"the module pattern must escape the dot: {contents:?}"
		);

		std::fs::remove_dir_all(dir).unwrap();
	}
}
