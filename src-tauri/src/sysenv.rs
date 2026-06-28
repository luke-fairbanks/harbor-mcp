//! System-environment helpers. A Finder-launched bundle inherits only a minimal
//! `PATH`, so we deterministically rebuild the directories where the user's
//! toolchains (nvm/Homebrew/asdf) and CLIs (e.g. `claude`) actually live, and
//! resolve binaries against them. Shell-free, so it never hangs.

use std::path::{Path, PathBuf};

fn add(dirs: &mut Vec<String>, p: String) {
    if !p.is_empty() && Path::new(&p).is_dir() && !dirs.contains(&p) {
        dirs.push(p);
    }
}

/// Candidate toolchain directories, most-specific first.
pub fn enriched_dirs() -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();

    // inherited PATH first (preserves an explicit dev setup)
    if let Ok(cur) = std::env::var("PATH") {
        for p in cur.split(':') {
            add(&mut dirs, p.to_string());
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        // nvm: the configured default version, then all installed (newest first)
        let nvm_node = Path::new(&home).join(".nvm/versions/node");
        if let Ok(def) = std::fs::read_to_string(Path::new(&home).join(".nvm/alias/default")) {
            let want = def.trim().trim_start_matches('v');
            add(
                &mut dirs,
                nvm_node
                    .join(format!("v{want}"))
                    .join("bin")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if let Ok(rd) = std::fs::read_dir(&nvm_node) {
            let mut versions: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            versions.sort();
            for v in versions.into_iter().rev() {
                add(&mut dirs, v.join("bin").to_string_lossy().into_owned());
            }
        }
        for p in [
            format!("{home}/.asdf/shims"),
            format!("{home}/.bun/bin"),
            format!("{home}/.local/bin"),
            format!("{home}/.cargo/bin"),
            format!("{home}/.claude/local"), // native Claude Code installer
        ] {
            add(&mut dirs, p);
        }
    }

    for p in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        add(&mut dirs, p.to_string());
    }

    dirs
}

/// The enriched `PATH` string (or `None` if somehow empty).
pub fn enriched_path() -> Option<String> {
    let dirs = enriched_dirs();
    if dirs.is_empty() {
        None
    } else {
        Some(dirs.join(":"))
    }
}

/// Find an executable named `name` across the enriched directories.
pub fn resolve_bin(name: &str) -> Option<PathBuf> {
    for d in enriched_dirs() {
        let p = Path::new(&d).join(name);
        if is_executable(&p) {
            return Some(p);
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
