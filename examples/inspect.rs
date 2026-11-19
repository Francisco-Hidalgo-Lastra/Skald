//! Read-only diagnostics: `cargo run --example inspect [archive...]`
//! Prints the detected game setup and, for each archive, its installer type.

use skald::{archive, config::Config, installer, plugins::LoadOrder};

fn main() -> anyhow::Result<()> {
    let cfg = Config::load();
    println!("game dir:   {:?}", cfg.game_dir);
    println!("data dir:   {:?}", cfg.data_path());
    println!("appdata:    {:?}", cfg.appdata_dir);
    if let (Some(g), Some(d), Some(a)) = (&cfg.game_dir, cfg.data_path(), &cfg.appdata_dir) {
        let lo = LoadOrder::load(a, &d, g, &[]);
        let active = lo.plugins.iter().filter(|p| p.enabled).count();
        println!("plugins:    {} found, {active} active", lo.plugins.len());
        for p in lo.plugins.iter().take(8) {
            println!("  {}{} {}", if p.enabled { "*" } else { " " }, p.name, if p.master { "[M]" } else { "" });
        }
    }
    for a in std::env::args().skip(1) {
        let tmp = tempfile_dir();
        archive::extract(a.as_ref(), &tmp)?;
        let kind = installer::detect(&tmp)?;
        println!("{a}: {}", kind.label());
        if let installer::InstallerKind::Fomod(p) = &kind {
            for s in &p.config.steps {
                println!("  step {:?}: {} groups", s.name, s.groups.len());
            }
        }
        let _ = std::fs::remove_dir_all(tmp);
    }
    Ok(())
}

fn tempfile_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("skald-inspect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
