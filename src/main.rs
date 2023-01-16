use std::process::Command;
use anyhow::Result;

use log::debug;

/// Gets the kernel version using `uname -r`
/// Works with `systemd-nspawn`, or alternatively `chroot` + `mount --bind` in docker
fn get_krnl_ver(target: String) -> Result<String> {
    let out = Command::new("rpm").args(["-q", "kernel", "--root"]).arg(target).output()?;
    Ok(String::from_utf8(out.stdout)?.strip_prefix("kernel-").unwrap().to_string())
}


// FIXME: The damn borrowing stuff just clone I don't care anymore
/// ```
/// /usr/bin/dracut --verbose --no-hostonly --no-hostonly-cmdline --install /.profile --add " kiwi-live pollcdrom " --omit " multipath " Ultramarine-Linux.x86_64-0.0.0.initrd 6.0.15-300.fc37.x86_64
/// ```
fn dracut(target: String) -> Result<()> {
    let raw = get_krnl_ver(target.clone())?;
    let ver = raw.clone();
    let mut ver = ver.split("-");
    let krnlver = ver.next().unwrap();
    let mut others = ver.next().unwrap().split(".");
    let xxx = others.next().unwrap();
    let fc = others.next().unwrap();
    let arch = others.next().unwrap();
    let out = Command::new("dracut").arg("--kernel-ver").arg(raw).arg("--sysroot").arg(target).args(["--verbose", "--no-hostonly", "--no-hostonly-cmdline", "--install", "/.profile", "--add", " kiwi-live pollcdrom ", "--omit", " multipath "]).arg(format!("Ultramarine-Linux.{arch}-{krnlver}.initrd")).arg(format!("{krnlver}-{xxx}.{fc}.{arch}")).status();
    if out.is_ok() {
        Ok(())
    } else {
        Err(out.err().unwrap().into())
    }
}

fn grub_mkconfig(target: String) -> Result<()> {
    let stat = Command::new("grub2-mkconfig").arg("-o").arg(format!("{target}boot/grub2/grub.cfg")).status();
    if stat.is_ok() {
        Ok(())
    } else {
        Err(stat.err().unwrap().into())
    }
}
fn grub_install(target: String, disk: String, arch: &str) -> Result<()> {
    let stat = Command::new("grub2-install").arg(disk).arg("--target").arg(arch).status();
    if stat.is_ok() {
        Ok(())
    } else {
        Err(stat.err().unwrap().into())
    }
}


fn main() {
    pretty_env_logger::init();
    debug!("カツ丼は最高！");
}
