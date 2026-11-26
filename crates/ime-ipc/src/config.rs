//! Persistent user configuration schema shared by `ime-server`, `ime-tsf`, and `ime-settings`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows_core::PCWSTR;

use crate::pipe::to_wide_null;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateLayout {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightStyle {
    #[default]
    Subtle,
    Inverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImeConfig {
    pub theme: ThemeMode,
    pub layout: CandidateLayout,
    pub highlight_style: HighlightStyle,
    pub font_family: String,
    pub font_size: i32,
    pub selected_weight: u16,
    pub page_size: usize,
    pub show_preedit_caret: bool,
    pub show_mode_toast: bool,
    pub default_chinese_punct: bool,
    pub shift_toggle: bool,
    pub initial_upper_english: bool,
    pub learning_enabled: bool,
    pub cs_ai_boost: bool,
}

impl Default for ImeConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Auto,
            layout: CandidateLayout::Horizontal,
            highlight_style: HighlightStyle::Subtle,
            font_family: String::new(),
            font_size: 15,
            selected_weight: 400,
            page_size: 5,
            show_preedit_caret: true,
            show_mode_toast: true,
            default_chinese_punct: true,
            shift_toggle: true,
            initial_upper_english: true,
            learning_enabled: true,
            cs_ai_boost: false,
        }
    }
}

impl ImeConfig {
    pub fn normalized(mut self) -> Self {
        self.font_size = self.font_size.clamp(12, 22);
        self.page_size = self.page_size.clamp(3, 9);
        self.selected_weight = if self.selected_weight <= 450 {
            400
        } else if self.selected_weight <= 550 {
            500
        } else {
            600
        };
        self.font_family = self.font_family.trim().to_owned();
        self
    }

    pub fn default_config_path() -> PathBuf {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            if !local_app_data.is_empty() {
                return PathBuf::from(local_app_data)
                    .join("Pyn")
                    .join("config.json");
            }
        }
        PathBuf::from("config.json")
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str::<Self>(&raw)
                .unwrap_or_default()
                .normalized(),
            Err(_) => Self::default(),
        }
    }

    pub fn save_atomic(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let norm = self.clone().normalized();
        let json = serde_json::to_string_pretty(&norm)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("json encode: {e}")))?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json)?;
        let wide_src = to_wide_null(&tmp_path.to_string_lossy());
        let wide_dst = to_wide_null(&path.to_string_lossy());
        unsafe {
            MoveFileExW(
                PCWSTR(wide_src.as_ptr()),
                PCWSTR(wide_dst.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_json_without_cs_ai_boost_defaults_to_false() {
        let legacy = r#"{"theme":"auto","layout":"horizontal","page_size":5,"learning_enabled":true}"#;
        let parsed = serde_json::from_str::<ImeConfig>(legacy).unwrap().normalized();
        assert!(!parsed.cs_ai_boost);
        assert!(parsed.learning_enabled);
        assert_eq!(parsed.page_size, 5);
    }
}
