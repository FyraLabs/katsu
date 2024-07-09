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

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
// load script from file, or inline if there's one specified
pub struct Script {
	pub id: Option<String>,
	pub name: Option<String>,
	pub file: Option<std::path::PathBuf>,
	pub inline: Option<String>,
	pub chroot: bool,
	#[serde(default)]
	pub needs: Vec<String>,
	/// Default 50, the higher, the later the script executes
	#[serde(default = "script_default_priority")]
	pub priority: i32,
}

impl Default for Script {
	fn default() -> Self {
		Script {
			id: None,
			name: None,
			file: None,
			inline: None,
			chroot: true,
			needs: Vec::new(),
			priority: 50,
		}
	}
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
	
	fn shebang_if_needed(&self) -> Option<String> {
	  self.load().and_then(|s| {
        if s.starts_with("#!") {
          None
        } else {
          Some(format!("#!/bin/sh\n{}", s))
        }
      })
	}

	pub fn execute(
		&self, container: &mut tiffin::Container,
	) -> Result<(), Box<dyn std::error::Error>> {
	    if self.chroot {
			tracing::trace!("chrooting to {:?}", container.root);
			
		}
		todo!()
	}
}
