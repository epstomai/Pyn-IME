//! Fixed and generation-versioned CLSID / TSF profile GUID constants for Pyn.
//!
//! When deployed as `ime_tsf_gNNNN.dll` / `ime_tsf_x86_gNNNN.dll`, every GUID
//! and candidate window class derives deterministically from the 4-hex generation
//! suffix so Windows TSF can hot-swap running applications without restarts.

use std::sync::OnceLock;

use windows_core::GUID;

use crate::globals::dll_module_path;

pub const CLSID_PYN_TIP: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0101);
pub const GUID_PYN_PROFILE: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0102);
pub const GUID_DISPLAY_ATTR_INPUT: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0103);
pub const GUID_PRESERVED_KEY_SHIFT: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0104);
pub const GUID_PRESERVED_KEY_PUNCT: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0105);

pub const LANGID_ZH_CN: u16 = 0x0804;
pub const TIP_DISPLAY_NAME: &str = "Pyn 拼音";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TipGuids {
    pub generation: u16,
    pub clsid: GUID,
    pub profile: GUID,
    pub display_attr_input: GUID,
    pub preserved_key_shift: GUID,
    pub preserved_key_punct: GUID,
}

pub fn parse_generation_from_filename(stem: &str) -> u16 {
    let lower = stem.to_ascii_lowercase();
    let Some((prefix, hex_part)) = lower.rsplit_once("_g") else {
        return 0;
    };
    if prefix != "ime_tsf" && prefix != "ime_tsf_x86" {
        return 0;
    }
    if hex_part.len() != 4 || !hex_part.bytes().all(|b| b.is_ascii_hexdigit()) {
        return 0;
    }
    u16::from_str_radix(hex_part, 16).unwrap_or(0)
}

pub fn guids_for_generation(generation: u16) -> TipGuids {
    if generation == 0 {
        return TipGuids {
            generation: 0,
            clsid: CLSID_PYN_TIP,
            profile: GUID_PYN_PROFILE,
            display_attr_input: GUID_DISPLAY_ATTR_INPUT,
            preserved_key_shift: GUID_PRESERVED_KEY_SHIFT,
            preserved_key_punct: GUID_PRESERVED_KEY_PUNCT,
        };
    }
    let gen_bits = generation as u128;
    TipGuids {
        generation,
        clsid: GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b20_6c5f8a3d0000 | gen_bits),
        profile: GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b21_6c5f8a3d0000 | gen_bits),
        display_attr_input: GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b22_6c5f8a3d0000 | gen_bits),
        preserved_key_shift: GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b23_6c5f8a3d0000 | gen_bits),
        preserved_key_punct: GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b24_6c5f8a3d0000 | gen_bits),
    }
}

static ACTIVE_GUIDS: OnceLock<TipGuids> = OnceLock::new();

pub fn active_guids() -> &'static TipGuids {
    ACTIVE_GUIDS.get_or_init(|| {
        let gen = dll_module_path()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map(|stem| parse_generation_from_filename(&stem))
            .unwrap_or(0);
        guids_for_generation(gen)
    })
}

pub fn guid_to_braced_string(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generation_filename_parsing_and_unique_guids() {
        assert_eq!(parse_generation_from_filename("ime_tsf"), 0);
        assert_eq!(parse_generation_from_filename("ime_tsf_x86"), 0);
        assert_eq!(parse_generation_from_filename("ime_tsf_g0000"), 0);
        assert_eq!(parse_generation_from_filename("ime_tsf-abc123"), 0);
        assert_eq!(parse_generation_from_filename("ime_tsf_g0001"), 1);
        assert_eq!(parse_generation_from_filename("ime_tsf_x86_g002A"), 0x002a);
        assert_eq!(parse_generation_from_filename("IME_TSF_X86_GFFFF"), 0xffff);
        assert_eq!(parse_generation_from_filename("ime_tsf_g123"), 0);
        assert_eq!(parse_generation_from_filename("ime_tsf_g12345"), 0);

        let g0 = guids_for_generation(0);
        assert_eq!(g0.clsid, CLSID_PYN_TIP);
        assert_eq!(g0.profile, GUID_PYN_PROFILE);

        let g1 = guids_for_generation(1);
        let g2 = guids_for_generation(2);
        assert_eq!(
            guid_to_braced_string(&g1.clsid),
            "{A94E2C1B-7D3F-4E8A-9B20-6C5F8A3D0001}"
        );
        assert_eq!(
            guid_to_braced_string(&g1.profile),
            "{A94E2C1B-7D3F-4E8A-9B21-6C5F8A3D0001}"
        );

        let mut seen = HashSet::new();
        for g in [g0, g1, g2] {
            for guid in [
                g.clsid,
                g.profile,
                g.display_attr_input,
                g.preserved_key_shift,
                g.preserved_key_punct,
            ] {
                assert!(seen.insert(guid_to_braced_string(&guid)));
            }
        }
    }
}
