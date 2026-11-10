//! Looker's version is the package's (`<Identity Version>` in Package.appxmanifest, bumped each release), not
//! Cargo's: it's handed to the code as `LOOKER_VERSION`, without the revision (always 0, or a dev-mode stamp).

fn main() {
    println!("cargo:rerun-if-changed=Package.appxmanifest");
    let manifest = std::fs::read_to_string("Package.appxmanifest").expect("Package.appxmanifest");
    let identity = &manifest[manifest.find("<Identity").expect("<Identity")..];
    let at = identity.find("Version=\"").expect("Identity Version") + "Version=\"".len();
    let full = &identity[at..at + identity[at..].find('"').expect("closing quote")];
    let parts: Vec<&str> = full.split('.').collect();
    let shown = parts[..parts.len().min(3)].join(".");
    println!("cargo:rustc-env=LOOKER_VERSION={shown}");
}
