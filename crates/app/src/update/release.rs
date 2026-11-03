//! Finding the newest release on GitHub and checking its signed manifest.
//!
//! Each release carries `nordpil-update.json`, listing its version and the
//! SHA-256 of every file, and `nordpil-update.json.sig`, an Ed25519
//! signature of it made in the release workflow. Only a manifest signed with
//! the key in `assets/update-key.pem` is trusted, and only for the version
//! it names, so an old release re-uploaded under a new tag is refused.

use std::io::Read;
use std::time::Duration;

use ed25519_dalek::pkcs8::DecodePublicKey as _;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::Deserialize;

const RELEASES_URL: &str =
    "https://api.github.com/repos/LajnaLegenden/nordpil/releases?per_page=20";
pub const MANIFEST_NAME: &str = "nordpil-update.json";
const SIGNATURE_NAME: &str = "nordpil-update.json.sig";
const PUBLIC_KEY: &str = include_str!("../../assets/update-key.pem");
/// Manifests and signatures are tiny; anything bigger isn't one.
const MAX_SMALL_BODY: u64 = 1 << 20;
/// The largest file an update may download.
pub const MAX_DOWNLOAD: u64 = 512 << 20;

/// A newer release, its manifest already verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release page on GitHub.
    pub page: String,
    /// The file for this install, when the release has one.
    pub download: Option<Download>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub name: String,
    pub url: String,
    pub sha256: [u8; 32],
}

#[derive(Debug, Deserialize)]
pub struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

impl GithubRelease {
    fn asset_url(&self, name: &str) -> Option<&str> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.as_str())
    }
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    version: String,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    name: String,
    sha256: String,
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(concat!("nordpil/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The newest release after `current`, if there is one. `asset` names the
/// file this install updates from, given the release's version.
pub fn check(
    agent: &ureq::Agent,
    current: &Version,
    asset: impl Fn(&Version) -> Option<String>,
) -> Result<Option<Release>, String> {
    let body = get(
        agent,
        RELEASES_URL,
        MAX_SMALL_BODY,
        "application/vnd.github+json",
    )?;
    let releases: Vec<GithubRelease> = serde_json::from_slice(&body)
        .map_err(|error| format!("GitHub sent an unexpected release list: {error}"))?;
    let Some((version, release)) = newest(&releases, current) else {
        return Ok(None);
    };
    // `newest` only picks releases that have both files.
    let manifest_url = release.asset_url(MANIFEST_NAME).unwrap_or_default();
    let signature_url = release.asset_url(SIGNATURE_NAME).unwrap_or_default();
    let manifest = get(agent, manifest_url, MAX_SMALL_BODY, "*/*")?;
    let signature = get(agent, signature_url, MAX_SMALL_BODY, "*/*")?;
    let manifest = verify(&manifest, &signature, &version)?;

    let download = asset(&version).and_then(|name| {
        let url = release.asset_url(&name)?.to_owned();
        let sha256 = manifest.sha256(&name)?;
        Some(Download { name, url, sha256 })
    });
    Ok(Some(Release {
        version,
        page: release.html_url.clone(),
        download,
    }))
}

/// The newest published release newer than `current` that can update it.
/// Prereleases are only offered to prereleases, so a stable install stays on
/// stable releases.
pub fn newest<'a>(
    releases: &'a [GithubRelease],
    current: &Version,
) -> Option<(Version, &'a GithubRelease)> {
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter(|release| !release.prerelease || !current.pre.is_empty())
        .filter(|release| {
            release.asset_url(MANIFEST_NAME).is_some()
                && release.asset_url(SIGNATURE_NAME).is_some()
        })
        .filter_map(|release| {
            let tag = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name);
            let version = Version::parse(tag).ok()?;
            (version > *current).then_some((version, release))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
}

/// Checks the signature and that the manifest is for `version`.
pub fn verify(manifest: &[u8], signature: &[u8], version: &Version) -> Result<Manifest, String> {
    let key = VerifyingKey::from_public_key_pem(PUBLIC_KEY)
        .map_err(|error| format!("The built-in update key is invalid: {error}"))?;
    verify_with(&key, manifest, signature, version)
}

fn verify_with(
    key: &VerifyingKey,
    manifest: &[u8],
    signature: &[u8],
    version: &Version,
) -> Result<Manifest, String> {
    let signature = Signature::from_slice(signature)
        .map_err(|_| "The update's signature is malformed.".to_owned())?;
    key.verify_strict(manifest, &signature)
        .map_err(|_| "The update's signature doesn't match. It was not installed.".to_owned())?;
    let manifest: Manifest = serde_json::from_slice(manifest)
        .map_err(|error| format!("The update's manifest is malformed: {error}"))?;
    if Version::parse(&manifest.version).ok().as_ref() != Some(version) {
        return Err(format!(
            "The update's manifest is for {}, not {version}.",
            manifest.version
        ));
    }
    Ok(manifest)
}

impl Manifest {
    fn sha256(&self, name: &str) -> Option<[u8; 32]> {
        let file = self.files.iter().find(|file| file.name == name)?;
        parse_hex(&file.sha256)
    }
}

fn parse_hex(text: &str) -> Option<[u8; 32]> {
    let text = text.as_bytes();
    if text.len() != 64 {
        return None;
    }
    let mut out = [0; 32];
    for (byte, pair) in out.iter_mut().zip(text.chunks(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

fn get(agent: &ureq::Agent, url: &str, limit: u64, accept: &str) -> Result<Vec<u8>, String> {
    let response = agent
        .get(url)
        .header("Accept", accept)
        .call()
        .map_err(|error| format!("Could not reach GitHub: {error}"))?;
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("Could not download from GitHub: {error}"))?;
    if body.len() as u64 > limit {
        return Err(format!("{url} is larger than expected."));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    fn release(tag: &str, prerelease: bool, signed: bool) -> GithubRelease {
        let mut assets = vec![GithubAsset {
            name: "nordpil-x.AppImage".into(),
            browser_download_url: "https://example.com/x".into(),
        }];
        if signed {
            for name in [MANIFEST_NAME, SIGNATURE_NAME] {
                assets.push(GithubAsset {
                    name: name.into(),
                    browser_download_url: format!("https://example.com/{name}"),
                });
            }
        }
        GithubRelease {
            tag_name: tag.into(),
            html_url: format!("https://example.com/{tag}"),
            draft: false,
            prerelease,
            assets,
        }
    }

    fn picked(releases: &[GithubRelease], current: &str) -> Option<String> {
        newest(releases, &Version::parse(current).unwrap()).map(|(version, _)| version.to_string())
    }

    #[test]
    fn picks_the_newest_signed_stable_release() {
        let releases = [
            release("v0.2.0", false, true),
            release("v0.4.0", false, false),
            release("v0.3.0-beta.1", true, true),
            release("v0.2.1", false, true),
            release("nightly", false, true),
        ];
        assert_eq!(picked(&releases, "0.1.0").as_deref(), Some("0.2.1"));
        assert_eq!(picked(&releases, "0.2.1"), None);
        // A prerelease install is offered newer prereleases too.
        assert_eq!(
            picked(&releases, "0.2.1-beta.1").as_deref(),
            Some("0.3.0-beta.1")
        );
    }

    #[test]
    fn skips_drafts() {
        let mut draft = release("v9.0.0", false, true);
        draft.draft = true;
        assert_eq!(picked(&[draft], "0.1.0"), None);
    }

    #[test]
    fn verifies_signature_and_version() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let hash = "ab".repeat(32);
        let manifest =
            format!(r#"{{"version":"0.2.0","files":[{{"name":"a.tar.gz","sha256":"{hash}"}}]}}"#);
        let signature = key.sign(manifest.as_bytes()).to_bytes();
        let public = key.verifying_key();
        let version = Version::parse("0.2.0").unwrap();

        let checked = verify_with(&public, manifest.as_bytes(), &signature, &version).unwrap();
        assert_eq!(checked.sha256("a.tar.gz"), Some([0xab; 32]));
        assert_eq!(checked.sha256("b.tar.gz"), None);

        let other = Version::parse("0.3.0").unwrap();
        let error = verify_with(&public, manifest.as_bytes(), &signature, &other).unwrap_err();
        assert!(error.contains("not 0.3.0"), "{error}");

        let tampered = manifest.replace("0.2.0", "0.2.1");
        assert!(verify_with(&public, tampered.as_bytes(), &signature, &version).is_err());
        let stranger = SigningKey::from_bytes(&[8; 32]).verifying_key();
        assert!(verify_with(&stranger, manifest.as_bytes(), &signature, &version).is_err());
    }

    #[test]
    fn built_in_key_parses() {
        VerifyingKey::from_public_key_pem(PUBLIC_KEY).unwrap();
    }
}
