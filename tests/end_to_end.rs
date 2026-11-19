//! Archive → detection → FOMOD wizard → staging → deploy.

use skald::config::DeployMethod;
use skald::deploy::Deployer;
use skald::fomod::{NoFiles, Wizard};
use skald::installer::{self, InstallerKind};
use skald::mods::{ModEntry, ModList};
use std::fs;
use std::io::Write;
use std::path::Path;

const CONFIG: &str = r#"<?xml version="1.0" encoding="UTF-16"?>
<config><moduleName>Armor Pack</moduleName>
<requiredInstallFiles><file source="Core\ArmorPack.esp" destination="ArmorPack.esp"/></requiredInstallFiles>
<installSteps order="Explicit"><installStep name="Textures">
 <optionalFileGroups><group name="Resolution" type="SelectExactlyOne"><plugins order="Explicit">
  <plugin name="2K"><description>Balanced</description><image path="fomod\images\2k.png"/>
   <files><folder source="Tex\2K" destination="textures\armor"/></files>
   <typeDescriptor><type name="Recommended"/></typeDescriptor></plugin>
  <plugin name="4K"><description>Big</description>
   <files><folder source="Tex\4K" destination="textures\armor"/></files>
   <typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
 </plugins></group></optionalFileGroups></installStep></installSteps></config>"#;

fn utf16le(s: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    s.encode_utf16().for_each(|u| v.extend_from_slice(&u.to_le_bytes()));
    v
}

fn make_zip(path: &Path) {
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("ArmorPack-123-1-0/fomod/ModuleConfig.xml", utf16le(CONFIG)),
        ("ArmorPack-123-1-0/fomod/info.xml", b"<fomod><Name>Armor Pack</Name><Version>1.0</Version><Author>Someone</Author></fomod>".to_vec()),
        ("ArmorPack-123-1-0/Core/ArmorPack.esp", b"TES4\0\0\0\0\0\0\0\0".to_vec()),
        ("ArmorPack-123-1-0/Tex/2K/Cuirass.dds", b"2k".to_vec()),
        ("ArmorPack-123-1-0/Tex/4K/Cuirass.dds", b"4k".to_vec()),
    ];
    for (name, data) in files {
        z.start_file(name, opts).unwrap();
        z.write_all(&data).unwrap();
    }
    z.finish().unwrap();
}

#[test]
fn fomod_zip_install_and_deploy() {
    let t = tempfile::tempdir().unwrap();
    let archive = t.path().join("ArmorPack-123-1-0.zip");
    make_zip(&archive);

    let extracted = t.path().join("x");
    skald::archive::extract(&archive, &extracted).unwrap();
    let InstallerKind::Fomod(pkg) = installer::detect(&extracted).unwrap() else { panic!("not detected as FOMOD") };
    assert_eq!(pkg.info.version.as_deref(), Some("1.0"));

    let mut w = Wizard::new(pkg.config.clone(), &NoFiles);
    assert_eq!(w.selected[0][0], vec![true, false]); // recommended 2K preselected
    w.set(0, 0, 1, true, &NoFiles); // pick 4K
    let (ops, missing) = installer::plan_fomod(&pkg, &w.files_to_install(&NoFiles));
    assert!(missing.is_empty());

    let base = t.path().join("skald");
    let mut list = ModList::load(&base).unwrap();
    installer::execute(&ops, &list.mod_dir("Armor Pack")).unwrap();
    list.mods.push(ModEntry {
        name: "Armor Pack".into(), enabled: true, installer: "FOMOD".into(),
        version: None, author: None, source: None, installed_at: 0,
    });

    let game = t.path().join("game");
    let data = game.join("Data");
    fs::create_dir_all(data.join("Textures")).unwrap();
    Deployer::new(&base).deploy(&list, &game, &data, DeployMethod::Hardlink).unwrap();
    assert!(data.join("ArmorPack.esp").exists());
    assert_eq!(fs::read_to_string(data.join("Textures/armor/Cuirass.dds")).unwrap(), "4k");
    assert!(!data.join("fomod").exists());
}
