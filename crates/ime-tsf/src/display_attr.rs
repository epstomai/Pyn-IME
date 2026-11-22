//! TSF Display Attribute implementation (dotted underline for composing pinyin).

use std::cell::Cell;

use windows::Win32::Foundation::{E_POINTER, S_FALSE};
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, IEnumTfDisplayAttributeInfo_Impl, ITfDisplayAttributeInfo,
    ITfDisplayAttributeInfo_Impl, TF_ATTR_INPUT, TF_CT_NONE, TF_DISPLAYATTRIBUTE, TF_LS_DOT,
};
use windows_core::{implement, BOOL, BSTR, Error, GUID, Result};

use crate::guids::GUID_DISPLAY_ATTR_INPUT;

#[implement(ITfDisplayAttributeInfo)]
pub struct InputDisplayAttributeInfo;

impl ITfDisplayAttributeInfo_Impl for InputDisplayAttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_DISPLAY_ATTR_INPUT)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Input Engine Preedit"))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(Error::from(E_POINTER));
        }
        unsafe {
            *pda = TF_DISPLAYATTRIBUTE {
                crText: windows::Win32::UI::TextServices::TF_DA_COLOR {
                    r#type: TF_CT_NONE,
                    ..Default::default()
                },
                crBk: windows::Win32::UI::TextServices::TF_DA_COLOR {
                    r#type: TF_CT_NONE,
                    ..Default::default()
                },
                lsStyle: TF_LS_DOT,
                fBoldLine: BOOL(0),
                crLine: windows::Win32::UI::TextServices::TF_DA_COLOR {
                    r#type: TF_CT_NONE,
                    ..Default::default()
                },
                bAttr: TF_ATTR_INPUT,
            };
        }
        Ok(())
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        Ok(())
    }
}

#[implement(IEnumTfDisplayAttributeInfo)]
pub struct EnumDisplayAttributeInfo {
    index: Cell<usize>,
}

impl EnumDisplayAttributeInfo {
    pub fn new() -> Self {
        Self {
            index: Cell::new(0),
        }
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for EnumDisplayAttributeInfo_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        let copy = EnumDisplayAttributeInfo {
            index: Cell::new(self.index.get()),
        };
        Ok(copy.into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if ulcount > 0 && rginfo.is_null() {
            return Err(Error::from(E_POINTER));
        }
        let mut fetched = 0u32;
        if ulcount > 0 && self.index.get() == 0 {
            let info: ITfDisplayAttributeInfo = InputDisplayAttributeInfo.into();
            unsafe {
                *rginfo = Some(info);
            }
            self.index.set(1);
            fetched = 1;
        }
        if !pcfetched.is_null() {
            unsafe {
                *pcfetched = fetched;
            }
        }
        if fetched < ulcount {
            Err(Error::from(S_FALSE))
        } else {
            Ok(())
        }
    }

    fn Reset(&self) -> Result<()> {
        self.index.set(0);
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        if ulcount > 0 && self.index.get() == 0 {
            self.index.set(1);
            if ulcount > 1 {
                return Err(Error::from(S_FALSE));
            }
        } else if ulcount > 0 {
            return Err(Error::from(S_FALSE));
        }
        Ok(())
    }
}
