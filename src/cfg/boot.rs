#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bootloader {
	#[default]
	#[serde(alias = "grub2")]
	Grub,
	Limine,
	SystemdBoot,
}
