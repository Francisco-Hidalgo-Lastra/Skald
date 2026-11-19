//! Reads the icon embedded in a Windows `.exe` (PE resources), for the tools tray.

use std::path::Path;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

/// An RGBA image.
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The exe's main icon, picking the size closest to `want` pixels. `None` if it
/// has no icon or isn't a readable PE file.
pub fn extract(exe: &Path, want: u32) -> Option<Icon> {
    let bytes = std::fs::read(exe).ok()?;
    let ico = to_ico(&bytes, want)?;
    let img = image::load_from_memory_with_format(&ico, image::ImageFormat::Ico).ok()?.into_rgba8();
    Some(Icon { width: img.width(), height: img.height(), rgba: img.into_raw() })
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

struct Pe<'a> {
    bytes: &'a [u8],
    /// (virtual address, virtual size, raw offset, raw size)
    sections: Vec<(u32, u32, u32, u32)>,
    /// File offset of the resource section's root directory.
    rsrc: usize,
}

impl<'a> Pe<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(bytes, 0x3C)? as usize;
        if bytes.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let nsections = u16_at(bytes, coff + 2)? as usize;
        let opt_size = u16_at(bytes, coff + 16)? as usize;
        let opt = coff + 20;
        let (count_at, dirs_at) = match u16_at(bytes, opt)? {
            0x10b => (92, 96),
            0x20b => (108, 112),
            _ => return None,
        };
        if u32_at(bytes, opt + count_at)? <= 2 {
            return None;
        }
        let rsrc_rva = u32_at(bytes, opt + dirs_at + 2 * 8)?;
        let sections = (0..nsections)
            .map(|i| {
                let s = opt + opt_size + i * 40;
                Some((u32_at(bytes, s + 12)?, u32_at(bytes, s + 8)?, u32_at(bytes, s + 20)?, u32_at(bytes, s + 16)?))
            })
            .collect::<Option<Vec<_>>>()?;
        let mut pe = Pe { bytes, sections, rsrc: 0 };
        pe.rsrc = pe.offset(rsrc_rva)?;
        (rsrc_rva != 0).then_some(pe)
    }

    fn offset(&self, rva: u32) -> Option<usize> {
        self.sections.iter().find_map(|&(va, vsize, raw, rsize)| {
            (rva >= va && rva < va + vsize.max(rsize)).then(|| (rva - va + raw) as usize)
        })
    }

    /// Entries of the resource directory at `dir` (relative to the resource
    /// root): (id or `None` if named, offset field).
    fn entries(&self, dir: usize) -> Vec<(Option<u32>, u32)> {
        let at = self.rsrc + dir;
        let (Some(named), Some(ids)) = (u16_at(self.bytes, at + 12), u16_at(self.bytes, at + 14)) else { return vec![] };
        (0..(named as usize + ids as usize))
            .filter_map(|i| {
                let e = at + 16 + i * 8;
                let name = u32_at(self.bytes, e)?;
                Some(((name & 0x8000_0000 == 0).then_some(name), u32_at(self.bytes, e + 4)?))
            })
            .collect()
    }

    fn subdir(&self, dir: usize, id: Option<u32>) -> Option<usize> {
        self.entries(dir)
            .into_iter()
            .find(|(i, off)| (id.is_none() || *i == id) && off & 0x8000_0000 != 0)
            .map(|(_, off)| (off & 0x7FFF_FFFF) as usize)
    }

    /// The data of the first language under a type/name directory.
    fn leaf(&self, name_dir: usize) -> Option<&'a [u8]> {
        let (_, off) = self.entries(name_dir).into_iter().find(|(_, off)| off & 0x8000_0000 == 0)?;
        let entry = self.rsrc + off as usize;
        let start = self.offset(u32_at(self.bytes, entry)?)?;
        let size = u32_at(self.bytes, entry + 4)? as usize;
        self.bytes.get(start..start + size)
    }
}

/// Rebuilds a one-image `.ico` file from the exe's first icon group.
fn to_ico(bytes: &[u8], want: u32) -> Option<Vec<u8>> {
    let pe = Pe::parse(bytes)?;
    let groups = pe.subdir(0, Some(RT_GROUP_ICON))?;
    let group = pe.leaf(pe.subdir(groups, None)?)?;
    let count = u16_at(group, 4)? as usize;
    // Each GRPICONDIRENTRY is 14 bytes; width/height 0 means 256.
    let best = (0..count)
        .filter_map(|i| {
            let e = group.get(6 + i * 14..6 + (i + 1) * 14)?;
            let w = if e[0] == 0 { 256 } else { e[0] as u32 };
            let bits = u16_at(e, 6)?;
            Some((e, w, bits))
        })
        .min_by_key(|&(_, w, bits)| (w.abs_diff(want), std::cmp::Reverse(bits)))?
        .0;
    let id = u16_at(best, 12)? as u32;
    let icons = pe.subdir(0, Some(RT_ICON))?;
    let data = pe.leaf(pe.subdir(icons, Some(id))?)?;

    let mut ico = Vec::with_capacity(22 + data.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&best[..8]);
    ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(data);
    Some(ico)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal PE32 with one icon group holding a 1x1 PNG icon.
    fn tiny_exe(png: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        let coff = 0x44;
        b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        b[coff + 16..coff + 18].copy_from_slice(&224u16.to_le_bytes());
        let opt = coff + 20;
        b[opt..opt + 2].copy_from_slice(&0x10bu16.to_le_bytes());
        b[opt + 92..opt + 96].copy_from_slice(&16u32.to_le_bytes());
        let rva = 0x1000u32;
        b[opt + 96 + 16..opt + 96 + 20].copy_from_slice(&rva.to_le_bytes());
        let sec = opt + 224;
        let raw = 0x200u32;

        // Resource section, offsets relative to its start.
        let mut r = vec![0u8; 0x100];
        let dir = |r: &mut Vec<u8>, at: usize, entries: &[(u32, u32)]| {
            r[at + 14..at + 16].copy_from_slice(&(entries.len() as u16).to_le_bytes());
            for (i, (id, off)) in entries.iter().enumerate() {
                r[at + 16 + i * 8..at + 20 + i * 8].copy_from_slice(&id.to_le_bytes());
                r[at + 20 + i * 8..at + 24 + i * 8].copy_from_slice(&off.to_le_bytes());
            }
        };
        let sub = 0x8000_0000;
        dir(&mut r, 0x00, &[(RT_ICON, sub | 0x20), (RT_GROUP_ICON, sub | 0x40)]);
        dir(&mut r, 0x20, &[(1, sub | 0x60)]);
        dir(&mut r, 0x40, &[(1, sub | 0x80)]);
        dir(&mut r, 0x60, &[(0x409, 0xA0)]);
        dir(&mut r, 0x80, &[(0x409, 0xB0)]);
        let mut group = vec![0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0];
        group.extend_from_slice(&(png.len() as u32).to_le_bytes());
        group.extend_from_slice(&1u16.to_le_bytes());
        let (g_at, i_at) = (0x100u32, 0x100 + group.len() as u32);
        r[0xA0..0xA4].copy_from_slice(&(rva + i_at).to_le_bytes());
        r[0xA4..0xA8].copy_from_slice(&(png.len() as u32).to_le_bytes());
        r[0xB0..0xB4].copy_from_slice(&(rva + g_at).to_le_bytes());
        r[0xB4..0xB8].copy_from_slice(&(group.len() as u32).to_le_bytes());
        r.extend_from_slice(&group);
        r.extend_from_slice(png);

        b[sec + 8..sec + 12].copy_from_slice(&(r.len() as u32).to_le_bytes());
        b[sec + 12..sec + 16].copy_from_slice(&rva.to_le_bytes());
        b[sec + 16..sec + 20].copy_from_slice(&(r.len() as u32).to_le_bytes());
        b[sec + 20..sec + 24].copy_from_slice(&raw.to_le_bytes());
        b.extend_from_slice(&r);
        b
    }

    #[test]
    fn reads_embedded_icon() {
        let mut png = vec![];
        image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("tool.exe");
        std::fs::write(&exe, tiny_exe(&png)).unwrap();
        let icon = extract(&exe, 32).unwrap();
        assert_eq!((icon.width, icon.height), (1, 1));
        assert_eq!(icon.rgba, [10, 20, 30, 255]);

        std::fs::write(&exe, b"not an exe").unwrap();
        assert!(extract(&exe, 32).is_none());
    }
}
