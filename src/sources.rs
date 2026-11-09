//! Getting kernel source onto the disk: kernel.org tarballs, or a local tree.
//!
//! Downloads shell out to `curl` (or `wget`). Bundling a TLS stack to fetch a
//! tarball would dwarf the rest of this program, and any machine that can build
//! a kernel already has one of the two.

use crate::json::{self, Json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const RELEASES_URL: &str = "https://www.kernel.org/releases.json";
pub const CDN: &str = "https://cdn.kernel.org/pub/linux/kernel";

/// Shown when kernel.org cannot be reached, so the UI still offers something.
const FALLBACK: &[(&str, &str)] = &[
    ("6.12.1", "longterm"),
    ("6.6.63", "longterm"),
    ("6.1.119", "longterm"),
    ("5.15.173", "longterm"),
];

pub fn tarball_url(version: &str) -> String {
    let major = version.split('.').next().unwrap_or("6");
    if major == "2" {
        format!("{CDN}/v2.6/linux-{version}.tar.xz")
    } else {
        format!("{CDN}/v{major}.x/linux-{version}.tar.xz")
    }
}

/// Reject anything that is not a plain dotted version, so a crafted string can
/// never reach the shell-free `Command` as a path component or URL.
pub fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() < 32
        && v.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
        && v.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && !v.contains("..")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fetch a URL's body with whatever HTTP client is installed.
pub fn fetch(url: &str, timeout_secs: u32) -> io::Result<Vec<u8>> {
    if crate::system::which("curl").is_some() {
        let out = Command::new("curl")
            .args([
                "-fsSL",
                "--max-time",
                &timeout_secs.to_string(),
                "-A",
                concat!("kertweaker/", env!("CARGO_PKG_VERSION")),
                url,
            ])
            .output()?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        return Err(io::Error::other(format!(
            "curl failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if crate::system::which("wget").is_some() {
        let out = Command::new("wget")
            .args(["-qO-", "-T", &timeout_secs.to_string(), url])
            .output()?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        return Err(io::Error::other("wget failed"));
    }
    Err(io::Error::other(
        "no curl or wget available to download with",
    ))
}

/// Available kernel versions from kernel.org, cached on disk for six hours.
pub fn releases(cache_dir: &Path) -> Json {
    let cache = cache_dir.join("releases.json");
    if let Ok(meta) = fs::metadata(&cache) {
        let age = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| now().saturating_sub(d.as_secs()))
            .unwrap_or(u64::MAX);
        if age < 6 * 3600 {
            if let Ok(text) = fs::read_to_string(&cache) {
                if let Ok(j) = json::parse(&text) {
                    return j;
                }
            }
        }
    }

    match fetch(RELEASES_URL, 20).and_then(|b| {
        json::parse(&String::from_utf8_lossy(&b)).map_err(io::Error::other)
    }) {
        Ok(data) => {
            let list: Vec<Json> = data
                .get("releases")
                .and_then(Json::as_array)
                .unwrap_or(&[])
                .iter()
                .filter(|r| r.get("version").and_then(Json::as_str).is_some())
                .filter(|r| r.str_or("moniker", "") != "linux-next")
                .map(|r| {
                    let v = r.str_or("version", "");
                    Json::obj(vec![
                        ("version", Json::s(v)),
                        ("moniker", Json::s(r.str_or("moniker", "stable"))),
                        (
                            "released",
                            Json::s(
                                r.get("released")
                                    .map(|d| d.str_or("isodate", ""))
                                    .unwrap_or(""),
                            ),
                        ),
                        ("eol", Json::b(r.bool_or("iseol", false))),
                        (
                            "url",
                            Json::s(match r.get("source").and_then(Json::as_str) {
                                Some(u) if !u.is_empty() => u.to_string(),
                                _ => tarball_url(v),
                            }),
                        ),
                    ])
                })
                .collect();
            let out = Json::obj(vec![
                ("online", Json::b(true)),
                ("fetched_at", Json::n(now() as f64)),
                (
                    "latest_stable",
                    Json::s(
                        data.get("latest_stable")
                            .map(|l| l.str_or("version", ""))
                            .unwrap_or(""),
                    ),
                ),
                ("releases", Json::arr(list)),
            ]);
            let _ = fs::create_dir_all(cache_dir);
            let _ = fs::write(&cache, out.to_string());
            out
        }
        Err(e) => {
            // Stale cache beats no list at all.
            if let Ok(text) = fs::read_to_string(&cache) {
                if let Ok(Json::Obj(mut pairs)) = json::parse(&text) {
                    pairs.push(("stale".into(), Json::b(true)));
                    return Json::Obj(pairs);
                }
            }
            Json::obj(vec![
                ("online", Json::b(false)),
                ("error", Json::s(e.to_string())),
                (
                    "releases",
                    Json::arr(
                        FALLBACK
                            .iter()
                            .map(|(v, m)| {
                                Json::obj(vec![
                                    ("version", Json::s(*v)),
                                    ("moniker", Json::s(*m)),
                                    ("eol", Json::b(false)),
                                    ("url", Json::s(tarball_url(v))),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        }
    }
}

/// Read `VERSION`/`PATCHLEVEL`/`SUBLEVEL` from a tree's top-level Makefile.
pub fn tree_version(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path.join("Makefile")).ok()?;
    let head: String = text.lines().take(60).collect::<Vec<_>>().join("\n");
    let field = |key: &str| -> Option<String> {
        head.lines()
            .find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == key).then(|| v.trim().to_string())
            })
    };
    let version = field("VERSION")?;
    let patch = field("PATCHLEVEL")?;
    if version.is_empty() || patch.is_empty() {
        return None;
    }
    let mut out = format!("{version}.{patch}");
    match field("SUBLEVEL") {
        Some(s) if !s.is_empty() => out.push_str(&format!(".{s}")),
        _ => {}
    }
    if let Some(extra) = field("EXTRAVERSION") {
        out.push_str(&extra);
    }
    Some(out)
}

pub fn is_kernel_tree(path: &Path) -> bool {
    path.join("Makefile").is_file()
        && path.join("kernel").is_dir()
        && path.join("Kbuild").exists()
        && tree_version(path).is_some()
}

/// Kernel trees already on this machine, so the user can build one they have.
pub fn local_trees(search_dirs: &[PathBuf]) -> Vec<Json> {
    let mut found = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in search_dirs {
        let mut candidates = vec![dir.clone()];
        if let Ok(entries) = fs::read_dir(dir) {
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            children.sort();
            candidates.extend(children);
        }
        for c in candidates {
            let Ok(real) = c.canonicalize() else { continue };
            if seen.contains(&real) || !is_kernel_tree(&real) {
                continue;
            }
            let version = tree_version(&real).unwrap_or_default();
            found.push(Json::obj(vec![
                ("path", Json::s(real.to_string_lossy())),
                ("version", Json::s(version)),
                ("has_config", Json::b(real.join(".config").is_file())),
            ]));
            seen.push(real);
        }
    }
    found
}

/// Directories worth scanning for an existing kernel tree.
pub fn default_search_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join("sources"), PathBuf::from("/usr/src")];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join("src"));
        dirs.push(home.join("kernel"));
        dirs.push(home);
    }
    dirs
}

/// Stream a tarball to `dest_dir`, reporting progress lines as curl emits them.
pub fn download(
    url: &str,
    dest_dir: &Path,
    log: &mut dyn FnMut(&str),
) -> io::Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let name = url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\']))
        .ok_or_else(|| io::Error::other("cannot derive a filename from the URL"))?;
    let dest = dest_dir.join(name);
    if dest.is_file() {
        let size = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        if size > 1024 {
            log(&format!(
                "Reusing downloaded tarball {} ({:.1} MiB)",
                dest.display(),
                size as f64 / 1024.0 / 1024.0
            ));
            return Ok(dest);
        }
    }
    let part = dest.with_extension("part");
    log(&format!("Downloading {url}"));
    let status = if crate::system::which("curl").is_some() {
        Command::new("curl")
            .args(["-fL", "--retry", "3", "--progress-bar", "-o"])
            .arg(&part)
            .arg(url)
            .status()?
    } else {
        Command::new("wget")
            .args(["-q", "--show-progress", "-O"])
            .arg(&part)
            .arg(url)
            .status()?
    };
    if !status.success() {
        let _ = fs::remove_file(&part);
        return Err(io::Error::other(format!("download failed: {url}")));
    }
    fs::rename(&part, &dest)?;
    log(&format!("Saved {}", dest.display()));
    Ok(dest)
}

/// Unpack a kernel tarball and return the extracted tree.
pub fn extract(tarball: &Path, dest_dir: &Path, log: &mut dyn FnMut(&str)) -> io::Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let stem = tarball
        .file_name()
        .map(|n| n.to_string_lossy().replace(".tar.xz", "").replace(".tar.gz", ""))
        .unwrap_or_default();
    let expected = dest_dir.join(&stem);
    if expected.is_dir() && is_kernel_tree(&expected) {
        log(&format!("Reusing extracted tree at {}", expected.display()));
        return Ok(expected);
    }

    log(&format!("Extracting {}", tarball.display()));
    let out = Command::new("tar")
        .arg("-xf")
        .arg(tarball)
        .arg("-C")
        .arg(dest_dir)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "tar failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if is_kernel_tree(&expected) {
        return Ok(expected);
    }
    // Fall back to whatever kernel-looking directory appeared.
    let mut entries: Vec<PathBuf> = fs::read_dir(dest_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_kernel_tree(p))
        .collect();
    entries.sort();
    entries
        .pop()
        .ok_or_else(|| io::Error::other("no kernel tree found after extraction"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarball_urls_follow_kernel_org_layout() {
        assert_eq!(
            tarball_url("6.12.1"),
            "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.1.tar.xz"
        );
        assert_eq!(
            tarball_url("5.15.173"),
            "https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-5.15.173.tar.xz"
        );
        assert!(tarball_url("2.6.32").contains("/v2.6/"));
    }

    #[test]
    fn version_validation_rejects_injection() {
        assert!(valid_version("6.12.1"));
        assert!(valid_version("6.1"));
        for bad in [
            "", "../../etc", "6.12.1; rm -rf /", "$(id)", "v6.12", "6.12.1/../x",
            "6..1", "&&whoami", "6.12.1 --upload-file",
        ] {
            assert!(!valid_version(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn tree_version_parses_a_makefile() {
        let dir = std::env::temp_dir().join(format!("kt-src-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(
            dir.join("Makefile"),
            "# SPDX\nVERSION = 6\nPATCHLEVEL = 12\nSUBLEVEL = 3\nEXTRAVERSION =\nNAME = Baby Opossum\n",
        )
        .unwrap();
        assert_eq!(tree_version(&dir).as_deref(), Some("6.12.3"));

        fs::write(
            dir.join("Makefile"),
            "VERSION = 6\nPATCHLEVEL = 13\nSUBLEVEL =\nEXTRAVERSION = -rc2\n",
        )
        .unwrap();
        assert_eq!(tree_version(&dir).as_deref(), Some("6.13-rc2"));

        // Not a kernel tree: no kernel/ directory.
        assert!(!is_kernel_tree(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tree_version_ignores_non_kernel_makefiles() {
        let dir = std::env::temp_dir().join(format!("kt-nope-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("Makefile"), "all:\n\tcargo build\n").unwrap();
        assert_eq!(tree_version(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn releases_always_returns_a_usable_list() {
        let dir = std::env::temp_dir().join(format!("kt-rel-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let j = releases(&dir);
        let list = j.get("releases").unwrap().as_array().unwrap();
        assert!(!list.is_empty());
        for r in list {
            assert!(!r.str_or("version", "").is_empty());
            assert!(r.str_or("url", "").starts_with("https://"));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
