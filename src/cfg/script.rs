use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct ScriptsManifest {
	#[serde(default)]
	pub pre: Vec<Script>,
	#[serde(default)]
	pub post: Vec<Script>,
}

fn script_default_priority() -> i32 {
	50
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq, Default)]
// load script from file, or inline if there's one specified
pub struct Script {
	pub id: Option<String>,
	pub name: Option<String>,
	pub file: Option<std::path::PathBuf>,
	pub inline: Option<String>,
	pub chroot: Option<bool>,
	#[serde(default)]
	pub needs: Vec<String>,
	/// Default 50, the higher, the later the script executes
	#[serde(default = "script_default_priority")]
	pub priority: i32,
}

impl Script {
	pub fn load(&self) -> Option<String> {
		if self.inline.is_some() {
			self.inline.clone()
		} else if let Some(f) = &self.file {
			std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok()
		} else {
			self.file
				.as_ref()
				.and_then(|f| std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok())
		}
	}
}
