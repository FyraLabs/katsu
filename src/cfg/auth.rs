use serde::{Deserialize, Serialize};

const fn _default_true() -> bool {
	true
}

/// Image default users configuration
#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Auth {
	/// Username for the user
	pub username: String,
	/// Passwords are optional, but heavily recommended
	/// Passwords must be hashed with crypt(3) or mkpasswd(1)
	pub password: Option<String>,
	/// Groups to add the user to
	#[serde(default)]
	pub groups: Vec<String>,
	/// Whether to create a home directory for the user
	/// Defaults to true
	#[serde(default = "_default_true")]
	pub create_home: bool,
	/// Shell for the user
	#[serde(default)]
	pub shell: Option<String>,
	/// UID for the user
	#[serde(default)]
	pub uid: Option<u32>,
	/// GID for the user
	#[serde(default)]
	pub gid: Option<u32>,

	/// SSH keys for the user
	/// This will be written to ~/.`ssh/authorized_keys`
	#[serde(default)]
	pub ssh_keys: Vec<String>,
}

impl Auth {
	pub fn add_to_chroot(&self, chroot: &std::path::Path) -> color_eyre::Result<()> {
		todo!()
	}
}
