// SPDX-License-Identifier: Apache-2.0
// Adapted from LiteParse 4d4a51c246ff56d382166930942898f3ff563eba.
//! Reverse-cmap recovery: parse an embedded sfnt (TrueType/OpenType) font
//! program's `cmap` table and invert it to a glyph-index → unicode map.
//!
//! Used when a font's /ToUnicode is missing or garbage AND its glyph names
//! are unavailable (typical of CID TrueType subsets). The embedded font's own
//! character map is the last structural source of truth tying glyphs back to
//! unicode. Pure Rust, no deps; only formats 4, 12, 6 and 0 are parsed (these
//! cover essentially all real-world fonts).

use std::collections::HashMap;

/// Build glyph_index → unicode from an sfnt font program. Returns None when
/// the data is not sfnt (e.g. bare CFF) or has no usable cmap subtable.
pub fn reverse_cmap(data: &[u8]) -> Option<HashMap<u32, u32>> {
    let directory = sfnt_directory(data)?;
    let cmap = find_table(data, directory, b"cmap")?;

    let num_subtables = read_u16(cmap, 2)? as usize;
    // Pick the best unicode subtable: full-repertoire (fmt 12) beats BMP (fmt 4).
    let mut best: Option<(u32, u32)> = None; // (score, offset)
    for i in 0..num_subtables {
        let rec = 4 + i * 8;
        let platform = read_u16(cmap, rec)?;
        let encoding = read_u16(cmap, rec + 2)?;
        let offset = read_u32(cmap, rec + 4)?;
        // Only true unicode subtables. Mac Roman (1,0) and symbol (3,0) cmaps
        // encode charcodes, not unicode — reversing them echoes garbage (e.g.
        // Wingdings (1,0) maps the checkmark glyph back to 'ü').
        let score = match (platform, encoding) {
            (3, 10) | (0, 4) | (0, 6) => 4, // UCS-4
            (3, 1) | (0, 0..=3) => 3,       // BMP
            _ => 0,
        };
        if score > 0 && best.is_none_or(|(s, _)| score > s) {
            best = Some((score, offset));
        }
    }
    let (_, offset) = best?;
    let remaining = cmap.get(offset as usize..)?;
    let format = read_u16(remaining, 0)?;
    // A malformed subtable must not borrow bytes from the following table to invent a valid mapping.
    let length = match format {
        0 | 4 | 6 => usize::from(read_u16(remaining, 2)?),
        12 => read_u32(remaining, 4)? as usize,
        _ => return None,
    };
    let sub = remaining.get(..length)?;

    let mut map: HashMap<u32, u32> = HashMap::new();
    let mut expanded = 0_usize;
    let mut add = |glyph: u32, unicode: u32| -> Option<()> {
        // Bound total work, including overlapping ranges, before trusting embedded font tables.
        expanded += 1;
        if expanded > 1_114_112 {
            return None;
        }
        if glyph == 0
            || !char::from_u32(unicode).is_some_and(|c| !c.is_control())
        {
            return Some(());
        }
        // On collision prefer non-PUA, then the smaller codepoint (ASCII /
        // canonical forms over compatibility duplicates).
        match map.entry(glyph) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(unicode);
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let cur = *e.get();
                let cur_pua = (0xE000..=0xF8FF).contains(&cur);
                let new_pua = (0xE000..=0xF8FF).contains(&unicode);
                if (cur_pua && !new_pua)
                    || (cur_pua == new_pua && unicode < cur)
                {
                    e.insert(unicode);
                }
            }
        }
        Some(())
    };

    match format {
        0 => {
            // Byte encoding table: 256 glyph ids at offset 6
            for code in 0..256u32 {
                let g = *sub.get(6 + code as usize)? as u32;
                add(g, code)?;
            }
        }
        4 => {
            let seg_count_x2 = read_u16(sub, 6)?;
            if seg_count_x2 == 0 || !seg_count_x2.is_multiple_of(2) {
                return None;
            }
            let seg_count = usize::from(seg_count_x2 / 2);
            let end_codes = 14;
            let start_codes = end_codes + seg_count * 2 + 2;
            let id_deltas = start_codes + seg_count * 2;
            let id_range_offsets = id_deltas + seg_count * 2;
            for seg in 0..seg_count {
                let end = read_u16(sub, end_codes + seg * 2)? as u32;
                let start = read_u16(sub, start_codes + seg * 2)? as u32;
                let delta = read_u16(sub, id_deltas + seg * 2)? as u32;
                let range_offset =
                    read_u16(sub, id_range_offsets + seg * 2)? as usize;
                if start == 0xFFFF && end == 0xFFFF {
                    continue;
                }
                for code in start..=end.min(0xFFFE) {
                    let glyph = if range_offset == 0 {
                        (code + delta) & 0xFFFF
                    } else {
                        // glyphIdArray indexing relative to this rangeOffset slot
                        let slot = id_range_offsets
                            + seg * 2
                            + range_offset
                            + (code - start) as usize * 2;
                        let g = read_u16(sub, slot)? as u32;
                        if g == 0 { 0 } else { (g + delta) & 0xFFFF }
                    };
                    add(glyph, code)?;
                }
            }
        }
        6 => {
            let first = read_u16(sub, 6)? as u32;
            let count = read_u16(sub, 8)? as usize;
            if first + count as u32 > 0x10000 {
                return None;
            }
            for i in 0..count {
                let g = read_u16(sub, 10 + i * 2)? as u32;
                add(g, first + i as u32)?;
            }
        }
        12 => {
            let n_groups = read_u32(sub, 12)? as usize;
            if n_groups > 100_000 {
                return None;
            }
            for i in 0..n_groups {
                let rec = 16 + i * 12;
                let start = read_u32(sub, rec)?;
                let end = read_u32(sub, rec + 4)?;
                let start_glyph = read_u32(sub, rec + 8)?;
                if end < start || end > 0x10FFFF {
                    return None;
                }
                for off in 0..=(end - start) {
                    add(
                        start_glyph.checked_add(off)?,
                        start.checked_add(off)?,
                    )?;
                }
            }
        }
        _ => return None,
    }

    if map.is_empty() { None } else { Some(map) }
}

/// Locates the first sfnt directory while keeping TTC table offsets relative to the complete file.
fn sfnt_directory(data: &[u8]) -> Option<usize> {
    let tag = data.get(..4)?;
    if tag == b"ttcf" {
        let first = read_u32(data, 12)? as usize;
        let m = data.get(first..first.checked_add(4)?)?;
        return (m == [0, 1, 0, 0] || m == b"OTTO" || m == b"true")
            .then_some(first);
    }
    (tag == [0, 1, 0, 0] || tag == b"OTTO" || tag == b"true").then_some(0)
}

/// Returns a bounds-checked table using absolute sfnt/TTC offsets.
fn find_table<'a>(
    font: &'a [u8],
    directory: usize,
    tag: &[u8; 4],
) -> Option<&'a [u8]> {
    let num_tables = read_u16(font, directory.checked_add(4)?)? as usize;
    for i in 0..num_tables {
        let rec = directory.checked_add(12 + i * 16)?;
        if font.get(rec..rec.checked_add(4)?)? == tag {
            let offset = read_u32(font, rec.checked_add(8)?)? as usize;
            let length = read_u32(font, rec.checked_add(12)?)? as usize;
            return font.get(offset..offset.checked_add(length)?);
        }
    }
    None
}

/// Reads a big-endian field only when both bytes are present.
fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

/// Reads a big-endian field only when all four bytes are present.
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal sfnt with a single format-4 cmap subtable mapping
    /// 'A'..='C' (0x41..0x43) to glyphs 10..12.
    fn minimal_format4_font() -> Vec<u8> {
        // format 4 subtable: one real segment + terminator segment
        let mut sub: Vec<u8> = Vec::new();
        sub.extend([0, 4]); // format
        sub.extend([0, 32]); // Complete two-segment format-4 subtable length.
        sub.extend([0, 0]); // language
        sub.extend([0, 4]); // segCountX2 = 4 (2 segments)
        sub.extend([0, 0, 0, 0, 0, 0]); // searchRange/entrySelector/rangeShift
        sub.extend([0x00, 0x43, 0xFF, 0xFF]); // endCodes
        sub.extend([0, 0]); // reservedPad
        sub.extend([0x00, 0x41, 0xFF, 0xFF]); // startCodes
        // idDelta: glyph = code + delta mod 65536; 10 - 0x41 = -55 = 0xFFC9
        sub.extend([0xFF, 0xC9, 0x00, 0x01]); // idDeltas
        sub.extend([0, 0, 0, 0]); // idRangeOffsets

        let mut cmap: Vec<u8> = Vec::new();
        cmap.extend([0, 0]); // version
        cmap.extend([0, 1]); // numTables
        cmap.extend([0, 3, 0, 1]); // platform 3, encoding 1
        cmap.extend(12u32.to_be_bytes()); // subtable offset
        cmap.extend(&sub);

        let mut font: Vec<u8> = Vec::new();
        font.extend([0, 1, 0, 0]); // sfnt version
        font.extend([0, 1]); // numTables
        font.extend([0, 0, 0, 0, 0, 0]); // search fields
        font.extend(b"cmap");
        font.extend([0, 0, 0, 0]); // checksum
        font.extend(28u32.to_be_bytes()); // offset (12 + 16)
        font.extend((cmap.len() as u32).to_be_bytes());
        font.extend(&cmap);
        font
    }

    /// Verifies parses format4 and inverts against literal expectations.
    #[test]
    fn parses_format4_and_inverts() {
        let font = minimal_format4_font();
        let map = reverse_cmap(&font).expect("valid Unicode cmap");
        assert_eq!(map.get(&10), Some(&0x41)); // A
        assert_eq!(map.get(&11), Some(&0x42)); // B
        assert_eq!(map.get(&12), Some(&0x43)); // C
        assert!(!map.contains_key(&0));
    }

    /// Verifies rejects non sfnt against literal expectations.
    #[test]
    fn rejects_non_sfnt() {
        assert!(reverse_cmap(b"%!PS-AdobeFont").is_none());
        assert!(reverse_cmap(&[1, 0, 0, 0]).is_none()); // bare CFF header
        assert!(reverse_cmap(&[]).is_none());
    }

    /// TTC offsets stay file-relative; truncated subtables and non-Unicode platforms cannot fabricate mappings.
    #[test]
    fn validates_collection_offsets_and_subtable_boundaries() {
        let font = minimal_format4_font();
        for length in 0..font.len() {
            assert!(
                reverse_cmap(font.get(..length).expect("prefix")).is_none()
            );
        }
        let mut symbol = font.clone();
        symbol
            .get_mut(34..36)
            .expect("encoding")
            .copy_from_slice(&0_u16.to_be_bytes());
        assert!(reverse_cmap(&symbol).is_none());
        let mut ttc = b"ttcf\0\x01\0\0\0\0\0\x01\0\0\0\x10".to_vec();
        ttc.extend(&font);
        ttc.get_mut(36..40)
            .expect("absolute cmap offset")
            .copy_from_slice(&44_u32.to_be_bytes());
        assert_eq!(reverse_cmap(&ttc).expect("TTC").get(&10), Some(&0x41));
    }

    /// Formats 0, 6 and 12 resolve literal codepoints and reject overflowing or oversized ranges.
    #[test]
    fn decodes_supported_cmap_formats_with_bounded_work() {
        let base = minimal_format4_font();
        let mut format0 = vec![0, 0, 1, 6, 0, 0];
        format0.resize(262, 0);
        *format0.get_mut(6 + 65).expect("ASCII A glyph") = 10;
        let format6 = vec![0, 6, 0, 12, 0, 0, 0, 65, 0, 1, 0, 10];
        let mut format12 =
            vec![0, 12, 0, 0, 0, 0, 0, 28, 0, 0, 0, 0, 0, 0, 0, 1];
        format12.extend(0x1F600_u32.to_be_bytes());
        format12.extend(0x1F600_u32.to_be_bytes());
        format12.extend(10_u32.to_be_bytes());
        for (subtable, expected) in
            [(format0, 65), (format6, 65), (format12.clone(), 0x1F600)]
        {
            let mut font =
                base.get(..40).expect("font and cmap headers").to_vec();
            font.get_mut(24..28).expect("cmap length").copy_from_slice(
                &(12_u32 + subtable.len() as u32).to_be_bytes(),
            );
            font.extend(subtable);
            assert_eq!(
                reverse_cmap(&font).expect("supported cmap").get(&10),
                Some(&expected)
            );
        }
        format12
            .get_mut(20..24)
            .expect("end")
            .copy_from_slice(&0x1F601_u32.to_be_bytes());
        format12
            .get_mut(24..28)
            .expect("start glyph")
            .copy_from_slice(&u32::MAX.to_be_bytes());
        let mut malformed = base.get(..40).expect("headers").to_vec();
        malformed
            .get_mut(24..28)
            .expect("cmap length")
            .copy_from_slice(&40_u32.to_be_bytes());
        malformed.extend(format12);
        assert!(reverse_cmap(&malformed).is_none());
    }
}
