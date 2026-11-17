fn main() {
    #[cfg(windows)]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let project_root = std::path::Path::new(&manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .unwrap();
        let icon_path = project_root.join("assets").join("icon.ico");
        let manifest_path = project_root.join("assets").join("app.manifest");

        if icon_path.exists() && manifest_path.exists() {
            let out_dir = std::env::var("OUT_DIR").unwrap();
            let res_path = format!("{}/app.o", out_dir);
            let rc_file = format!("{}/app.rc", out_dir);

            let icon_str = icon_path.to_string_lossy().replace('\\', "/");
            let manifest_str = manifest_path.to_string_lossy().replace('\\', "/");

            let rc_content = format!("1 ICON \"{}\"\n1 24 \"{}\"\n", icon_str, manifest_str);
            let _ = std::fs::write(&rc_file, rc_content);

            if std::process::Command::new("windres")
                .args(["-i", &rc_file, "-o", &res_path])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                println!("cargo:rustc-link-arg={}", res_path);
            }
        }
        println!("cargo:rerun-if-changed=../../assets/icon.ico");
        println!("cargo:rerun-if-changed=../../assets/app.manifest");
    }
}
