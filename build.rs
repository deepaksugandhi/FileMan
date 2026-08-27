fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        // Embed a Windows VERSIONINFO resource derived from Cargo's package
        // metadata, so the exe's file properties — and anything reading them
        // (e.g. the Inno Setup script's version lookup) — always reflect
        // Cargo.toml. Cargo.toml `[package] version` stays the single source
        // of truth; nothing else hardcodes it.
        let mut res = winresource::WindowsResource::new();

        // Default the human-readable strings to the bare three-part version
        // (no trailing ".0" build part) so consumers get "0.1.1", not
        // "0.1.1.0".
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set("FileDescription", "FileMan file manager");
        res.set("ProductName", "FileMan");
        res.set_icon("assets/app.ico");

        res.compile()
            .expect("failed to compile Windows version info resource");
    }
}
