//! Fixed CLSID and TSF profile GUID constants for Input Engine.

use windows_core::GUID;

pub const CLSID_INPUT_ENGINE_TIP: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0101);
pub const GUID_INPUT_ENGINE_PROFILE: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0102);
pub const GUID_DISPLAY_ATTR_INPUT: GUID = GUID::from_u128(0xa94e2c1b_7d3f_4e8a_9b12_6c5f8a3d0103);

pub const LANGID_ZH_CN: u16 = 0x0804;
pub const TIP_DISPLAY_NAME: &str = "Input Engine 拼音";

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
