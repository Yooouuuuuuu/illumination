// SPDX-License-Identifier: GPL-3.0-only
//! The name of the system this desk runs, as another desk shows it on this one's card:
//! "Windows 11", "Ubuntu 22.04", "macOS 15". The name and the main version, nothing more.
//! It is for the person's eye only; nothing in the program depends on it.

use std::sync::OnceLock;

/// The longest name kept or shown.
pub const LONGEST: usize = 32;

/// This machine's system. Found once per run.
pub fn name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| clean(&find()))
}

/// What another desk said its system is, made fit to keep and show: one short line.
pub fn clean(said: &str) -> String {
    said.chars()
        .filter(|c| !c.is_control())
        .take(LONGEST)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(windows)]
fn find() -> String {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: asked from a windowed program, no console may flash up.
    let said = std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .creation_flags(0x0800_0000)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    windows_from_ver(&said)
}

/// From what `ver` prints, e.g. "Microsoft Windows [Version 10.0.26200.6584]". Windows 11
/// still calls itself version 10; it is told by its build number.
pub fn windows_from_ver(said: &str) -> String {
    let build = said
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|part| part.matches('.').count() >= 2)
        .and_then(|version| version.split('.').nth(2)?.parse::<u32>().ok());
    match build {
        Some(build) if build >= 22_000 => "Windows 11".to_string(),
        Some(build) if build >= 10_240 => "Windows 10".to_string(),
        _ => "Windows".to_string(),
    }
}

#[cfg(target_os = "macos")]
fn find() -> String {
    let version = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    match version.trim().split('.').next() {
        Some(major) if !major.is_empty() => format!("macOS {major}"),
        _ => "macOS".to_string(),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn find() -> String {
    std::fs::read_to_string("/etc/os-release")
        .map(|text| linux_from_os_release(&text))
        .unwrap_or_else(|_| "Linux".to_string())
}

/// From `/etc/os-release`: the distribution and its version, "Ubuntu 22.04"; a distribution
/// without releases (Arch) has no version and shows as its name alone.
pub fn linux_from_os_release(text: &str) -> String {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches('"').to_string())
            .filter(|value| !value.is_empty())
    };
    match (field("NAME"), field("VERSION_ID")) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name,
        _ => "Linux".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_is_told_by_its_build() {
        assert_eq!(
            windows_from_ver("\r\nMicrosoft Windows [Version 10.0.26200.6584]\r\n"),
            "Windows 11"
        );
        // Other languages word it differently; the number is the same.
        assert_eq!(
            windows_from_ver("Microsoft Windows [版本 10.0.19045.5011]"),
            "Windows 10"
        );
        assert_eq!(windows_from_ver(""), "Windows");
    }

    #[test]
    fn linux_shows_its_distribution() {
        let ubuntu = "PRETTY_NAME=\"Ubuntu 22.04.4 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"22.04\"\n";
        assert_eq!(linux_from_os_release(ubuntu), "Ubuntu 22.04");
        assert_eq!(
            linux_from_os_release("NAME=\"Arch Linux\"\nID=arch\n"),
            "Arch Linux"
        );
        assert_eq!(linux_from_os_release(""), "Linux");
    }

    #[test]
    fn this_machine_has_a_name_and_strangers_are_cut_short() {
        assert!(!name().is_empty());
        assert_eq!(clean("Ubuntu 22.04\n\u{7}"), "Ubuntu 22.04");
        assert_eq!(clean(&"x".repeat(200)).len(), LONGEST);
    }
}
