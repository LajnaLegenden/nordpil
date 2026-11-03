//! How nordpil was installed, and replacing that install with a newer one.
//!
//! An AppImage, a binary from the Linux tar.gz and nordpil.app on macOS can
//! replace themselves when their folder is writable. The new files are
//! unpacked next to the old ones and renamed into place, so a failed
//! download never leaves a half-written app. A .deb or a root-owned install
//! is left to the user: the banner links to the release instead.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use semver::Version;
use sha2::{Digest as _, Sha256};

use super::release::{Download, MAX_DOWNLOAD};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// The AppImage file, from `$APPIMAGE`.
    AppImage(PathBuf),
    /// The binary unpacked from the Linux tar.gz.
    Binary(PathBuf),
    /// The nordpil.app bundle on macOS.
    Bundle(PathBuf),
    /// Can't be replaced from here, e.g. a .deb or an install under /usr/local.
    Manual,
}

impl Install {
    pub fn detect() -> Self {
        let Ok(exe) = std::env::current_exe() else {
            return Self::Manual;
        };
        let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
        let install = classify(&exe, appimage, std::env::consts::OS);
        let writable = match &install {
            Self::AppImage(path) | Self::Binary(path) | Self::Bundle(path) => {
                path.parent().is_some_and(can_write)
            }
            Self::Manual => true,
        };
        if writable { install } else { Self::Manual }
    }

    /// The release file this install updates from.
    pub fn asset(&self, version: &Version) -> Option<String> {
        let arch = std::env::consts::ARCH;
        match self {
            Self::AppImage(_) => Some(format!("nordpil-{version}-linux-{arch}.AppImage")),
            Self::Binary(_) => Some(format!("nordpil-{version}-linux-{arch}.tar.gz")),
            Self::Bundle(_) => {
                let arch = if arch == "aarch64" { "arm64" } else { arch };
                Some(format!("nordpil-{version}-macos-{arch}.app.tar.gz"))
            }
            Self::Manual => None,
        }
    }

    pub fn can_update(&self) -> bool {
        *self != Self::Manual
    }

    /// Downloads `download`, checks its hash and puts it in place of this
    /// install. The running app keeps working until it restarts.
    pub fn update(&self, agent: &ureq::Agent, download: &Download) -> Result<(), String> {
        let target = match self {
            Self::AppImage(path) | Self::Binary(path) | Self::Bundle(path) => path,
            Self::Manual => return Err("This install can't update itself.".into()),
        };
        let folder = target
            .parent()
            .ok_or_else(|| format!("{} has no parent folder.", target.display()))?;
        let staging = Staging::new(folder)?;
        let file = staging.path.join(&download.name);
        fetch(agent, download, &file)?;
        match self {
            Self::AppImage(path) => {
                set_executable(&file)?;
                rename(&file, path)
            }
            Self::Binary(path) => {
                untar(&file, &staging.path)?;
                let folder = download.name.trim_end_matches(".tar.gz");
                let binary = staging.path.join(folder).join("nordpil");
                set_executable(&binary)?;
                rename(&binary, path)
            }
            Self::Bundle(path) => {
                untar(&file, &staging.path)?;
                let bundle = staging.path.join("nordpil.app");
                if !bundle.join("Contents/MacOS/nordpil").is_file() {
                    return Err("The download doesn't hold nordpil.app.".into());
                }
                // Directories can't be renamed over each other: move the old
                // one aside first and put it back if the new one won't go.
                let old = staging.path.join("old.app");
                rename(path, &old)?;
                if let Err(error) = rename(&bundle, path) {
                    let _ = std::fs::rename(&old, path);
                    return Err(error);
                }
                Ok(())
            }
            Self::Manual => unreachable!(),
        }
    }

    /// Starts the installed (new) version. The caller then closes this one.
    pub fn relaunch(&self) -> Result<(), String> {
        let spawned = match self {
            Self::AppImage(path) | Self::Binary(path) => Command::new(path).spawn(),
            Self::Bundle(path) => Command::new("open").arg("-n").arg(path).spawn(),
            Self::Manual => return Err("This install can't restart itself.".into()),
        };
        spawned
            .map(drop)
            .map_err(|error| format!("Could not start the new version: {error}"))
    }
}

/// What `exe` is, ignoring whether it can be written.
fn classify(exe: &Path, appimage: Option<PathBuf>, os: &str) -> Install {
    match os {
        "linux" => {
            if let Some(appimage) = appimage.filter(|path| path.is_absolute()) {
                return Install::AppImage(appimage);
            }
            // Package managers own these, even when we could write them.
            let managed = [
                "/usr/bin",
                "/usr/sbin",
                "/usr/lib",
                "/bin",
                "/opt",
                "/nix",
                "/snap",
            ];
            if managed.iter().any(|prefix| exe.starts_with(prefix)) {
                return Install::Manual;
            }
            Install::Binary(exe.to_owned())
        }
        "macos" => {
            // .../nordpil.app/Contents/MacOS/nordpil
            let bundle = exe.parent().and_then(Path::parent).and_then(Path::parent);
            match bundle {
                Some(bundle)
                    if bundle.extension().is_some_and(|ext| ext == "app")
                        && exe.parent().is_some_and(|dir| dir.ends_with("Contents/MacOS"))
                        // Gatekeeper runs unmoved downloads from a read-only copy.
                        && !bundle.to_string_lossy().contains("/AppTranslocation/")
                        && !bundle.starts_with("/Volumes") =>
                {
                    Install::Bundle(bundle.to_owned())
                }
                _ => Install::Manual,
            }
        }
        _ => Install::Manual,
    }
}

fn can_write(folder: &Path) -> bool {
    let probe = folder.join(format!(".nordpil-write-test-{}", std::process::id()));
    let ok = File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// A hidden folder next to the install, on the same file system so its
/// contents can be renamed into place. Removed when dropped.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn new(folder: &Path) -> Result<Self, String> {
        let path = folder.join(format!(".nordpil-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path)
            .map_err(|error| format!("Could not write to {}: {error}", folder.display()))?;
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Downloads to `to`, failing unless the file has the expected SHA-256.
fn fetch(agent: &ureq::Agent, download: &Download, to: &Path) -> Result<(), String> {
    let response = agent
        .get(&download.url)
        .call()
        .map_err(|error| format!("Could not download {}: {error}", download.name))?;
    let mut reader = response.into_body().into_reader().take(MAX_DOWNLOAD + 1);
    let mut file =
        File::create(to).map_err(|error| format!("Could not save the download: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("The download of {} broke off: {error}", download.name))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_DOWNLOAD {
            return Err(format!("{} is larger than expected.", download.name));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|error| format!("Could not save the download: {error}"))?;
    }
    file.sync_all()
        .map_err(|error| format!("Could not save the download: {error}"))?;
    if hasher.finalize().as_slice() != download.sha256 {
        return Err(format!(
            "{} doesn't match its checksum. It was not installed.",
            download.name
        ));
    }
    Ok(())
}

fn untar(archive: &Path, into: &Path) -> Result<(), String> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|error| format!("Could not run tar: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not unpack the update: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("Could not make {} executable: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to)
        .map_err(|error| format!("Could not replace {}: {error}", to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_installs() {
        let path = PathBuf::from;
        assert_eq!(
            classify(
                &path("/tmp/.mount_x/usr/bin/nordpil"),
                Some(path("/home/me/Apps/nordpil.AppImage")),
                "linux"
            ),
            Install::AppImage(path("/home/me/Apps/nordpil.AppImage"))
        );
        assert_eq!(
            classify(&path("/home/me/.local/bin/nordpil"), None, "linux"),
            Install::Binary(path("/home/me/.local/bin/nordpil"))
        );
        assert_eq!(
            classify(&path("/usr/bin/nordpil"), None, "linux"),
            Install::Manual
        );
        assert_eq!(
            classify(
                &path("/Applications/nordpil.app/Contents/MacOS/nordpil"),
                None,
                "macos"
            ),
            Install::Bundle(path("/Applications/nordpil.app"))
        );
        assert_eq!(
            classify(
                &path(
                    "/private/var/folders/x/AppTranslocation/y/d/nordpil.app/Contents/MacOS/nordpil"
                ),
                None,
                "macos"
            ),
            Install::Manual
        );
        assert_eq!(
            classify(&path("/Users/me/bin/nordpil"), None, "macos"),
            Install::Manual
        );
        assert_eq!(
            classify(&path("C:\\nordpil.exe"), None, "windows"),
            Install::Manual
        );
    }

    #[test]
    fn names_release_files() {
        let version = Version::parse("0.3.0-beta.1").unwrap();
        let arch = std::env::consts::ARCH;
        assert_eq!(
            Install::Binary(PathBuf::from("/x")).asset(&version),
            Some(format!("nordpil-0.3.0-beta.1-linux-{arch}.tar.gz"))
        );
        assert_eq!(Install::Manual.asset(&version), None);
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_binary_from_a_tarball() {
        let dir = std::env::temp_dir().join(format!("nordpil-update-{}", uuid::Uuid::new_v4()));
        let packed = dir.join("packed/nordpil-9.9.9-linux-test");
        std::fs::create_dir_all(&packed).unwrap();
        std::fs::write(packed.join("nordpil"), b"new").unwrap();
        let archive = dir.join("nordpil-9.9.9-linux-test.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("packed"))
            .arg("nordpil-9.9.9-linux-test")
            .status()
            .unwrap();
        assert!(status.success());

        let staging = Staging::new(&dir).unwrap();
        untar(&archive, &staging.path).unwrap();
        let binary = staging.path.join("nordpil-9.9.9-linux-test/nordpil");
        set_executable(&binary).unwrap();
        let installed = dir.join("nordpil");
        std::fs::write(&installed, b"old").unwrap();
        rename(&binary, &installed).unwrap();
        let staged = staging.path.clone();
        drop(staging);

        assert_eq!(std::fs::read(&installed).unwrap(), b"new");
        assert!(!staged.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
