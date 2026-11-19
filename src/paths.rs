//! Path helpers. Skyrim (and every mod author) assumes a case-insensitive
//! filesystem with `\` separators; Linux is neither, so everything that touches
//! mod files goes through these helpers.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Turn a Windows-ish relative path from an archive/FOMOD into a safe, relative
/// `PathBuf`. Strips leading separators, drops `.` and rejects `..` components.
pub fn normalize_rel(s: &str) -> PathBuf {
    let s = s.replace('\\', "/");
    let mut out = PathBuf::new();
    for part in s.split('/') {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        out.push(part);
    }
    out
}

/// Lower-cased, `/`-separated key used to compare relative paths.
pub fn key(rel: &Path) -> String {
    let mut s = String::new();
    for c in rel.components() {
        if let Component::Normal(p) = c {
            if !s.is_empty() {
                s.push('/');
            }
            s.push_str(&p.to_string_lossy().to_lowercase());
        }
    }
    s
}

/// Find a direct child of `dir` whose name matches `name` case-insensitively.
pub fn find_child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if fs::symlink_metadata(&exact).is_ok() {
        return Some(exact);
    }
    let lower = name.to_lowercase();
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().to_lowercase() == lower)
        .map(|e| e.path())
}

/// Resolve `rel` under `base`, matching each existing component
/// case-insensitively. Components that don't exist are kept as given.
pub fn resolve_ci(base: &Path, rel: &Path) -> PathBuf {
    let mut cur = base.to_path_buf();
    let mut exists = true;
    for c in rel.components() {
        let Component::Normal(name) = c else { continue };
        let name = name.to_string_lossy();
        if exists {
            if let Some(found) = find_child_ci(&cur, &name) {
                cur = found;
                continue;
            }
            exists = false;
        }
        cur.push(name.as_ref());
    }
    cur
}

/// Like [`resolve_ci`] but creates missing parent directories, recording every
/// directory it created in `created` (outermost first).
pub fn resolve_ci_create(
    base: &Path,
    rel: &Path,
    created: &mut Vec<PathBuf>,
) -> std::io::Result<PathBuf> {
    let comps: Vec<_> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let mut cur = base.to_path_buf();
    for (i, name) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        match find_child_ci(&cur, name) {
            Some(found) if last || found.is_dir() => cur = found,
            _ => {
                cur.push(name);
                if !last {
                    fs::create_dir(&cur)?;
                    created.push(cur.clone());
                }
            }
        }
    }
    Ok(cur)
}

/// Recursively copy `src` (file or directory) to `dst_rel` under `dst_base`,
/// merging directories case-insensitively.
pub fn copy_merge(src: &Path, dst_base: &Path, dst_rel: &Path) -> std::io::Result<usize> {
    let mut scratch = Vec::new();
    if src.is_dir() {
        let mut n = 0;
        for entry in walkdir::WalkDir::new(src).min_depth(1) {
            let entry = entry?;
            if entry.file_type().is_dir() {
                continue;
            }
            let rel = entry.path().strip_prefix(src).unwrap();
            let target = resolve_ci_create(dst_base, &dst_rel.join(rel), &mut scratch)?;
            fs::copy(entry.path(), &target)?;
            n += 1;
        }
        Ok(n)
    } else {
        let target = resolve_ci_create(dst_base, dst_rel, &mut scratch)?;
        fs::copy(src, &target)?;
        Ok(1)
    }
}

/// Every regular file under `dir`, as paths relative to `dir`.
pub fn list_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .flatten()
        .filter(|e| !e.file_type().is_dir())
        .map(|e| e.path().strip_prefix(dir).unwrap().to_path_buf())
        .collect()
}

/// Make a string safe to use as a single directory name.
pub fn sanitize_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == '/' || c == '\0' || c == '\\' { '_' } else { c })
        .collect();
    let t = cleaned.trim().trim_matches('.').to_string();
    if t.is_empty() { "Unnamed Mod".into() } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_junk() {
        assert_eq!(normalize_rel("\\Data\\..\\Meshes\\.\\a.nif"), PathBuf::from("Data/Meshes/a.nif"));
        assert_eq!(key(Path::new("Meshes/Foo.NIF")), "meshes/foo.nif");
    }

    #[test]
    fn ci_resolution_merges() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("Textures/Actors")).unwrap();
        let mut created = vec![];
        let p = resolve_ci_create(t.path(), Path::new("textures/actors/new/x.dds"), &mut created).unwrap();
        assert_eq!(p, t.path().join("Textures/Actors/new/x.dds"));
        assert_eq!(created, vec![t.path().join("Textures/Actors/new")]);
    }
}
