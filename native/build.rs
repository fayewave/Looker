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

    exe_icon();
}

/// Puts `assets/AppIcon.ico` (made by `scripts/Generate-Logos.ps1`) into the exe as its icon resource. Explorer reads
/// the badge on video and document thumbnails from the exe named in a packaged desktop app's open command, not
/// from the package's logos: without this it drew Windows' blank program icon. Written as a `.res` straight to the
/// linker, so no resource compiler (rc.exe) is needed.
fn exe_icon() {
    println!("cargo:rerun-if-changed=assets/AppIcon.ico");
    let ico = std::fs::read("assets/AppIcon.ico").expect("assets/AppIcon.ico");
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    assert!(u16_at(0) == 0 && u16_at(2) == 1, "AppIcon.ico isn't an icon file");
    let count = u16_at(4) as usize;

    // A .res is a run of entries (header + data, each DWORD-aligned), starting with an empty one.
    fn entry(res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
        for v in [data.len() as u32, 32] {
            res.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0xFFFF, kind, 0xFFFF, id] {
            res.extend_from_slice(&u16::to_le_bytes(v));
        }
        res.extend_from_slice(&0u32.to_le_bytes()); // data version
        res.extend_from_slice(&flags.to_le_bytes());
        res.extend_from_slice(&0u16.to_le_bytes()); // language neutral
        res.extend_from_slice(&[0; 8]); // version, characteristics
        res.extend_from_slice(data);
        while res.len() % 4 != 0 {
            res.push(0);
        }
    }
    let mut res = Vec::new();
    entry(&mut res, 0, 0, 0, &[]);
    // RT_GROUP_ICON: the .ico's directory with each image's file offset replaced by its RT_ICON id.
    let mut group = ico[..6].to_vec();
    for i in 0..count {
        let dir = 6 + 16 * i;
        let (size, offset) = (u32_at(dir + 8) as usize, u32_at(dir + 12) as usize);
        entry(&mut res, 3, i as u16 + 1, 0x1010, &ico[offset..offset + size]);
        group.extend_from_slice(&ico[dir..dir + 12]);
        group.extend_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    entry(&mut res, 14, 1, 0x1030, &group);

    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("looker-icon.res");
    std::fs::write(&path, res).expect("write looker-icon.res");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}
