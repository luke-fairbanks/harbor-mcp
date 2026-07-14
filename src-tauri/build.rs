fn main() {
    const BRIDGE_MANIFEST: &str = "mcp-bridge/Cargo.toml";
    println!("cargo:rerun-if-changed={BRIDGE_MANIFEST}");
    let manifest =
        std::fs::read_to_string(BRIDGE_MANIFEST).expect("read native MCP bridge Cargo.toml");
    let mut in_package = false;
    let bridge_version = manifest
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line == "[package]" {
                in_package = true;
                return None;
            }
            if in_package && line.starts_with('[') {
                in_package = false;
            }
            in_package
                .then(|| line.strip_prefix("version = \"")?.strip_suffix('"'))
                .flatten()
        })
        .expect("native MCP bridge package version");
    println!("cargo:rustc-env=HARBOR_MCP_BRIDGE_VERSION={bridge_version}");
    tauri_build::build()
}
