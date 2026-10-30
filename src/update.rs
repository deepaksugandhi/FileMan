//! GitHub release update check. Uses the `curl.exe` that ships with
//! Windows 10+ so we don't pull in an HTTP/TLS stack for one request.

use std::os::windows::process::CommandExt;
use std::process::Command;

const API: &str = "https://api.github.com/repos/deepaksugandhi/FileMan/releases/latest";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub notes: String,
}

fn curl() -> Command {
    let mut c = Command::new("curl.exe");
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

fn parse(s: &str) -> Vec<u64> {
    s.trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    parse(latest) > parse(current)
}

/// Returns the latest release if it is newer than the running build and
/// ships a `*-setup.exe` asset.
pub fn check() -> Result<Option<Release>, String> {
    let out = curl()
        .args(["-sfL", "--max-time", "15", "-H", "User-Agent: FileMan", API])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !out.status.success() {
        return Err("Couldn't reach GitHub".into());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("Bad response: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("No release found")?;
    if !is_newer(tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let url = v["assets"]
        .as_array()
        .and_then(|a| {
            a.iter().find_map(|x| {
                let u = x["browser_download_url"].as_str()?;
                u.ends_with("-setup.exe").then(|| u.to_string())
            })
        })
        .ok_or("Release has no installer attached")?;
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        url,
        notes: v["body"].as_str().unwrap_or("").to_string(),
    }))
}

/// Downloads the installer to %TEMP% and launches it. Caller should exit
/// the app afterwards so the installer can replace the exe.
pub fn download_and_launch(url: &str) -> Result<(), String> {
    let dest = std::env::temp_dir().join("FileMan-update-setup.exe");
    let st = curl()
        .args(["-sfL", "--max-time", "600", "-o"])
        .arg(&dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !st.success() {
        return Err("Download failed".into());
    }
    Command::new(&dest)
        .spawn()
        .map_err(|e| format!("Couldn't start installer: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.18", "0.1.17"));
        assert!(is_newer("0.2.0", "0.1.17"));
        assert!(is_newer("0.1.100", "0.1.99"));
        assert!(!is_newer("0.1.17", "0.1.17"));
        assert!(!is_newer("v0.1.16", "0.1.17"));
    }
}
