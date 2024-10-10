use color_eyre::{Report, Result, Section};
use lazy_format::lazy_format as lzf;
use serde::{Deserialize, Serialize};
use std::{
	hash::{Hash, Hasher},
	io::Write,
	path::Path,
	process::Command,
};

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct ScriptsManifest {
	#[serde(default)]
	pub pre: Vec<Script>,
	#[serde(default)]
	pub post: Vec<Script>,
}

const fn script_default_priority() -> i32 {
	50
}

pub fn sort_script_priority(scripts: &mut [Script]) {
	scripts.sort_by_key(|s| s.priority);
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
// load script from file, or inline if there's one specified
pub struct Script {
	pub id: Option<String>,
	pub name: Option<String>,
	// pub file: Option<std::path::PathBuf>,
	pub source: Option<String>,
	pub chroot: bool,
	#[serde(default)]
	pub needs: Vec<String>,
	/// Default 50, the higher, the later the script executes
	#[serde(default = "script_default_priority")]
	pub priority: i32,
}

impl Default for Script {
	fn default() -> Self {
		Self { id: None, name: None, source: None, chroot: true, needs: Vec::new(), priority: 50 }
	}
}

/// # Errors
/// - cannot create tempfile
fn tmpfile_script(name: &str) -> std::io::Result<tempfile::NamedTempFile> {
	tempfile::Builder::new().prefix("katsu-script").suffix(name).tempfile()
}

impl Script {
	#[must_use]
	pub fn load(&self) -> Option<String> {
		self.source.clone()
		// if self.inline.is_some() {
		// 	self.inline.clone()
		// } else if let Some(f) = &self.file {
		// 	std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok()
		// } else {
		// 	self.file
		// 		.as_ref()
		// 		.and_then(|f| std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok())
		// }
	}

	fn shebang_if_needed(&self) -> Option<String> {
		self.load().map(|s| if s.starts_with("#!") { s } else { format!("#!/bin/sh\n{s}") })
	}

	fn get_id(&self) -> String {
		self.id.clone().unwrap_or_else(|| {
			let mut hasher = std::hash::DefaultHasher::new();
			self.source.hash(&mut hasher);
			hasher.finish().to_string()
		})
	}

	#[tracing::instrument(skip(container))]
	pub fn execute(&self, container: &mut tiffin::Container) -> Result<()> {
		let Some(script) = self.shebang_if_needed() else {
			return Err(Report::msg("Fail to load undefined script that cannot be found"));
		};
		// todo: generate random id if not provided? let's do that in ensan eval though, state caching stuff
		let tmpfile_name = format!("katsu-script-{}", self.get_id());
		if self.chroot {
			tracing::trace!("chrooting to {:?}", container.root);
			container.run(|| Self::_write_and_execute(&tmpfile_name, &script, None))??;
		} else {
			Self::_write_and_execute(&tmpfile_name, &script, Some(&container.root))?;
		}
		todo!()
	}

	/// Write the script to a temporary file and execute it.
	#[tracing::instrument]
	fn _write_and_execute(
		tmpfile_name: &str, script: &str, chroot: Option<&Path>,
	) -> Result<(), ScriptError> {
		let mut tmpfile = tmpfile_script(tmpfile_name)?;
		{
			let f = tmpfile.as_file_mut();
			f.write_all(script.as_bytes())?;
		};
		let mut cmd = Command::new(tmpfile.path());
		if let Some(chroot) = chroot {
			cmd.env("CHROOT", chroot);
		}
		let status = cmd.status()?;
		if status.success() {
			return Ok(());
		}
		if let Some(rc) = status.code() {
			return Err(Report::msg("Script exited")
				.warning(lzf!("Status code: {rc}"))
				.note("Status: {status}")
				.into());
		}
		Err(Report::msg("Script terminated unexpectedly").note(lzf!("Status: {status}")).into())
	}
}

#[derive(thiserror::Error, Debug)]
enum ScriptError {
	#[error("IO Error: {0}")]
	Io(#[from] std::io::Error),
	#[error("Error while running script: {0}")]
	Eyre(#[from] color_eyre::Report),
}
