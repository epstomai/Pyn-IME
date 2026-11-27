//! Platform-neutral mobile IME controller wrapping `ime-core` + `ime-ipc::session`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ime_core::decoder::{decode_rich_with_user, decode_with_user, CandidateKind, DecoderConfig};
use ime_core::user_dict::{LearnConfig, LearnDelta, UserMemory};
use ime_core::{Model, Reranker};
use ime_ipc::protocol::{WireCandidate, WireCandidateKind};
use ime_ipc::session::{InputMode, KeyAction, KeyEvent, KeyModifiers, SessionController};
use serde::Serialize;

const MAX_MOBILE_CANDIDATES: usize = 120;

#[derive(Debug, Clone, Serialize)]
pub struct ActionPayload {
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_text: Option<String>,
    pub undo_previous: bool,
    pub composing: bool,
    pub mode: &'static str,
    pub inline_preedit: String,
    pub panel_preedit: String,
    pub candidates: Vec<String>,
    pub single_char_mode: bool,
    pub latency_ms: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitPayload {
    pub ok: bool,
    pub load_ms: f32,
    pub reranker_loaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsPayload {
    pub ready: bool,
    pub reranker_loaded: bool,
    pub use_reranker: bool,
    pub learning_enabled: bool,
    pub cs_ai_boost: bool,
    pub total_steps: u64,
    pub active_vocab_words: usize,
    pub active_oov_words: usize,
    pub staged_words: usize,
    pub active_bigrams: usize,
    pub last_latency_ms: f32,
}

pub struct MobileEngine {
    model: Arc<Model>,
    reranker: Option<Arc<Reranker>>,
    user: UserMemory,
    user_dict_path: Option<PathBuf>,
    user_dirty: bool,
    session: SessionController,
    decoder_cfg: DecoderConfig,
    use_reranker: bool,
    last_keys: String,
    last_base_top1: Option<String>,
    last_current_top1: Option<String>,
    last_delta: Option<LearnDelta>,
    last_latency_ms: f32,
    /// Set when a keystroke updated the composition but its decode was deferred (another letter
    /// was already queued). Anything that reads candidates refreshes them first.
    candidates_stale: bool,
}

impl MobileEngine {
    pub fn load(
        model_path: &Path,
        reranker_path: Option<&Path>,
        user_dict_path: Option<&Path>,
    ) -> Result<(Self, f32), String> {
        let started = Instant::now();
        let model = Model::load(model_path)
            .map_err(|e| format!("failed to load model {}: {e}", model_path.display()))?;
        let model = Arc::new(model);

        let reranker = match reranker_path {
            Some(p) if p.is_file() => match Reranker::load(p) {
                Ok(r) => Some(Arc::new(r)),
                Err(_) => None,
            },
            _ => None,
        };

        let learn_cfg = LearnConfig::default();
        let user = match user_dict_path {
            Some(p) if p.is_file() => {
                UserMemory::load(p, &model, learn_cfg).unwrap_or_else(|_| UserMemory::new(learn_cfg))
            }
            _ => UserMemory::new(learn_cfg),
        };

        let mut session = SessionController::new();
        session.set_page_size(9);
        session.set_shift_toggle_enabled(false);
        session.set_initial_upper_english(false);

        let engine = Self {
            model,
            reranker,
            user,
            user_dict_path: user_dict_path.map(Path::to_path_buf),
            user_dirty: false,
            session,
            decoder_cfg: DecoderConfig::default(),
            use_reranker: true,
            last_keys: String::new(),
            last_base_top1: None,
            last_current_top1: None,
            last_delta: None,
            last_latency_ms: 0.0,
            candidates_stale: false,
        };
        let load_ms = started.elapsed().as_secs_f32() * 1000.0;
        Ok((engine, load_ms))
    }

    pub fn from_parts(model: Arc<Model>, reranker: Option<Arc<Reranker>>) -> Self {
        let mut session = SessionController::new();
        session.set_page_size(9);
        session.set_shift_toggle_enabled(false);
        session.set_initial_upper_english(false);
        Self {
            model,
            reranker,
            user: UserMemory::new(LearnConfig::default()),
            user_dict_path: None,
            user_dirty: false,
            session,
            decoder_cfg: DecoderConfig::default(),
            use_reranker: true,
            last_keys: String::new(),
            last_base_top1: None,
            last_current_top1: None,
            last_delta: None,
            last_latency_ms: 0.0,
            candidates_stale: false,
        }
    }

    pub fn has_reranker(&self) -> bool {
        self.reranker.is_some()
    }

    pub fn set_settings(
        &mut self,
        use_reranker: bool,
        learning_enabled: bool,
        cs_ai_boost: bool,
        chinese_punct: bool,
    ) {
        self.use_reranker = use_reranker;
        self.user.set_enabled(learning_enabled);
        self.decoder_cfg.cs_ai_boost = cs_ai_boost;
        self.session.set_chinese_punct(chinese_punct);
    }

    /// Re-decode if a deferred keystroke left the candidate list behind the composition.
    fn ensure_fresh(&mut self, context_before: &str) {
        if self.candidates_stale {
            self.candidates_stale = false;
            if self.session.is_composing() {
                self.refresh_candidates(context_before);
            }
        }
    }

    pub fn set_mode(&mut self, is_chinese: bool, context_before: &str) -> ActionPayload {
        self.ensure_fresh(context_before);
        let target = if is_chinese {
            InputMode::Chinese
        } else {
            InputMode::English
        };
        if self.session.mode() == target {
            return self.snapshot("consumed", None, false, 0.0);
        }
        // If switching from Chinese to English while composing, commit raw letters cleanly.
        if !is_chinese && self.session.is_composing() {
            let raw = self.session.commit_raw_for_passthrough();
            self.session.set_mode(target);
            self.clear_last_convert();
            return self.snapshot("commit", raw, false, 0.0);
        }
        self.session.set_mode(target);
        if is_chinese && self.session.is_composing() {
            let ms = self.refresh_candidates(context_before);
            return self.snapshot("update", None, false, ms);
        }
        self.snapshot("consumed", None, false, 0.0)
    }

    pub fn handle_char(
        &mut self,
        ch: char,
        is_upper: bool,
        context_before: &str,
    ) -> ActionPayload {
        self.handle_char_ext(ch, is_upper, context_before, true)
    }

    /// `decode = false` defers candidate generation (the caller already has another letter
    /// queued); the composition and inline preedit are still updated.
    pub fn handle_char_ext(
        &mut self,
        ch: char,
        is_upper: bool,
        context_before: &str,
        decode: bool,
    ) -> ActionPayload {
        // Plain letters only extend the keys; anything else may commit, so it needs fresh candidates.
        if !(ch.is_ascii_lowercase() && !is_upper) {
            self.ensure_fresh(context_before);
        }
        let event = if is_upper && ch.is_ascii_alphabetic() {
            KeyEvent::UpperChar(ch.to_ascii_uppercase())
        } else if ch.is_ascii_alphabetic() {
            KeyEvent::Char(ch.to_ascii_lowercase())
        } else {
            KeyEvent::Char(ch)
        };
        let mods = KeyModifiers {
            shift: is_upper,
            ..KeyModifiers::default()
        };
        let prev_char = context_before.chars().last();
        let action = self
            .session
            .handle_key_with_context(event, mods, prev_char);
        self.finish_action_ext(action, context_before, decode)
    }

    pub fn handle_special_key(&mut self, key_kind: i32, context_before: &str) -> ActionPayload {
        self.ensure_fresh(context_before);
        let event = match key_kind {
            0 => KeyEvent::Backspace,
            1 => KeyEvent::Space,
            2 => KeyEvent::Enter,
            3 => KeyEvent::Escape,
            4 => KeyEvent::Char('\''),
            5 => KeyEvent::Tab,
            _ => KeyEvent::Other,
        };
        let prev_char = context_before.chars().last();
        let action = self
            .session
            .handle_key_with_context(event, KeyModifiers::default(), prev_char);
        self.finish_action(action, context_before)
    }

    pub fn pick_candidate(&mut self, index: usize, context_before: &str) -> ActionPayload {
        self.ensure_fresh(context_before);
        let action = self.session.pick_candidate(index);
        self.finish_action(action, context_before)
    }

    pub fn commit_raw(&mut self) -> ActionPayload {
        if let Some(raw) = self.session.commit_raw_for_passthrough() {
            self.clear_last_convert();
            self.snapshot("commit", Some(raw), false, 0.0)
        } else {
            self.snapshot("consumed", None, false, 0.0)
        }
    }

    pub fn commit_with_suffix(&mut self, suffix: &str, context_before: &str) -> ActionPayload {
        self.ensure_fresh(context_before);
        if !self.session.is_composing() {
            self.last_delta = None;
            return self.snapshot("commit", Some(suffix.to_owned()), false, 0.0);
        }
        // Pick the best full candidate if available, else commit raw composing text + suffix.
        let action = if !self.session.all_candidates().is_empty() {
            self.session.handle_key_with_context(
                KeyEvent::Space,
                KeyModifiers::default(),
                context_before.chars().last(),
            )
        } else {
            let raw = self.session.commit_raw_for_passthrough().unwrap_or_default();
            KeyAction::Commit {
                text: raw,
                learn_keys: None,
                learn_text: None,
            }
        };
        match action {
            KeyAction::Commit {
                mut text,
                learn_keys,
                learn_text,
            } => {
                if let (Some(lk), Some(lt)) = (learn_keys, learn_text) {
                    self.record_learning(&lk, &lt, context_before);
                }
                text.push_str(suffix);
                self.snapshot("commit", Some(text), false, 0.0)
            }
            _ => {
                let mut raw = self.session.commit_raw_for_passthrough().unwrap_or_default();
                raw.push_str(suffix);
                self.clear_last_convert();
                self.snapshot("commit", Some(raw), false, 0.0)
            }
        }
    }

    pub fn reset_session(&mut self) {
        self.candidates_stale = false;
        self.session.on_focus_change();
        self.user.flush_compose_run(&self.model);
        self.clear_last_convert();
        self.last_delta = None;
    }

    pub fn save_user_dict(&mut self) -> bool {
        if !self.user_dirty {
            return true;
        }
        let Some(ref path) = self.user_dict_path else {
            return false;
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let tmp = path.with_extension("tsv.tmp");
        if self.user.save(&tmp).is_ok() && std::fs::rename(&tmp, path).is_ok() {
            self.user_dirty = false;
            true
        } else {
            false
        }
    }

    pub fn stats(&self) -> StatsPayload {
        let st = self.user.stats();
        StatsPayload {
            ready: true,
            reranker_loaded: self.reranker.is_some(),
            use_reranker: self.use_reranker,
            learning_enabled: self.user.config().enabled,
            cs_ai_boost: self.decoder_cfg.cs_ai_boost,
            total_steps: st.total_steps,
            active_vocab_words: st.active_vocab_words,
            active_oov_words: st.active_oov_words,
            staged_words: st.staged_words,
            active_bigrams: st.active_bigrams,
            last_latency_ms: self.last_latency_ms,
        }
    }

    fn clear_last_convert(&mut self) {
        self.last_keys.clear();
        self.last_base_top1 = None;
        self.last_current_top1 = None;
    }

    fn finish_action(&mut self, action: KeyAction, context_before: &str) -> ActionPayload {
        self.finish_action_ext(action, context_before, true)
    }

    fn finish_action_ext(
        &mut self,
        action: KeyAction,
        context_before: &str,
        decode: bool,
    ) -> ActionPayload {
        match action {
            KeyAction::PassThrough => self.snapshot("pass_through", None, false, 0.0),
            KeyAction::PassThroughAndUndo => {
                if let Some(delta) = self.last_delta.take() {
                    self.user.undo_commit(delta);
                    self.user_dirty = true;
                }
                self.snapshot("pass_through", None, true, 0.0)
            }
            KeyAction::ConsumedNoChange => self.snapshot("consumed", None, false, 0.0),
            KeyAction::UpdateComposition { need_convert } => {
                let ms = if need_convert && decode {
                    self.candidates_stale = false;
                    self.refresh_candidates(context_before)
                } else {
                    if need_convert {
                        self.candidates_stale = true;
                    }
                    0.0
                };
                self.snapshot("update", None, false, ms)
            }
            KeyAction::Commit {
                text,
                learn_keys,
                learn_text,
            } => {
                if let (Some(lk), Some(lt)) = (learn_keys, learn_text) {
                    self.record_learning(&lk, &lt, context_before);
                } else {
                    self.last_delta = None;
                }
                self.clear_last_convert();
                self.snapshot("commit", Some(text), false, 0.0)
            }
            KeyAction::CancelComposition => {
                self.clear_last_convert();
                self.snapshot("cancel", None, false, 0.0)
            }
        }
    }

    fn refresh_candidates(&mut self, context_before: &str) -> f32 {
        let keys = self.session.keys().to_owned();
        if keys.is_empty() {
            self.session.set_candidates(Vec::new());
            self.clear_last_convert();
            return 0.0;
        }
        let effective_context = if self.session.has_locked() {
            format!("{}{}", context_before, self.session.locked_text())
        } else {
            context_before.to_owned()
        };
        let active_reranker = if self.use_reranker {
            self.reranker.as_deref()
        } else {
            None
        };
        let started = Instant::now();
        let outcome = decode_rich_with_user(
            &self.model,
            active_reranker,
            Some(&self.user),
            &keys,
            &effective_context,
            MAX_MOBILE_CANDIDATES,
            &self.decoder_cfg,
        );
        let latency_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.last_latency_ms = latency_ms;

        let items: Vec<WireCandidate> = outcome
            .items
            .into_iter()
            .map(|it| WireCandidate {
                text: it.text,
                consumed_len: it.consumed_len,
                kind: match it.kind {
                    CandidateKind::Full => WireCandidateKind::Full,
                    CandidateKind::PrefixWord => WireCandidateKind::PrefixWord,
                    CandidateKind::PrefixChar => WireCandidateKind::PrefixChar,
                },
            })
            .collect();

        self.last_keys = keys;
        self.last_base_top1 = outcome.base_top1;
        self.last_current_top1 = items.first().map(|c| c.text.clone());

        self.session.set_rich_candidates(
            items,
            Some(outcome.segmented_preedit),
            outcome.first_char_offset,
            outcome.first_syllable_len,
        );
        latency_ms
    }

    fn record_learning(&mut self, keys: &str, text: &str, context_before: &str) {
        let (base_top1, cur_top1) = if self.last_keys == keys && !keys.is_empty() {
            (self.last_base_top1.take(), self.last_current_top1.take())
        } else if !keys.is_empty() {
            let active_reranker = if self.use_reranker {
                self.reranker.as_deref()
            } else {
                None
            };
            let outcome = decode_with_user(
                &self.model,
                active_reranker,
                Some(&self.user),
                keys,
                context_before,
                10,
                &self.decoder_cfg,
            );
            (outcome.base_top1, outcome.candidates.first().cloned())
        } else {
            (None, None)
        };

        let delta = self.user.record_commit(
            &self.model,
            keys,
            text,
            context_before,
            base_top1.as_deref(),
            cur_top1.as_deref(),
        );
        self.last_delta = Some(delta);
        self.user_dirty = true;
    }

    fn snapshot(
        &self,
        action: &'static str,
        commit_text: Option<String>,
        undo_previous: bool,
        latency_ms: f32,
    ) -> ActionPayload {
        let mode = match self.session.mode() {
            InputMode::Chinese => "zh",
            InputMode::English => "en",
        };
        ActionPayload {
            action,
            commit_text,
            undo_previous,
            composing: self.session.is_composing(),
            mode,
            inline_preedit: self.session.inline_preedit(),
            panel_preedit: self.session.panel_preedit(),
            candidates: self.session.all_candidates().to_vec(),
            single_char_mode: self.session.is_single_char_mode(),
            latency_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn deferred_decode_commits_like_immediate_decode() {
        let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("models")
            .join("ime_model.bin");
        if !model_path.is_file() {
            return;
        }
        let model = Arc::new(ime_core::Model::load(&model_path).expect("load model"));
        let rr_path = model_path.with_file_name("reranker.bin");
        let reranker = rr_path
            .is_file()
            .then(|| Arc::new(ime_core::Reranker::load(&rr_path).expect("load reranker")));

        for keys in ["ceshi", "jialichuang", "womenyaojiangdaode"] {
            let mut eager = MobileEngine::from_parts(model.clone(), reranker.clone());
            let mut lazy = MobileEngine::from_parts(model.clone(), reranker.clone());
            let mut last_eager = None;
            for (i, ch) in keys.chars().enumerate() {
                last_eager = Some(eager.handle_char(ch, false, ""));
                // Every letter but the last defers its decode, as when keys are queued.
                let lazy_res = lazy.handle_char_ext(ch, false, "", i + 1 == keys.len());
                assert_eq!(lazy_res.inline_preedit, last_eager.as_ref().unwrap().inline_preedit);
            }
            assert_eq!(lazy.snapshot("update", None, false, 0.0).candidates, last_eager.unwrap().candidates);

            // Deferred all the way, then space: the commit must still use fresh candidates.
            let mut lazy_all = MobileEngine::from_parts(model.clone(), reranker.clone());
            for ch in keys.chars() {
                lazy_all.handle_char_ext(ch, false, "", false);
            }
            let a = eager.handle_special_key(1, "");
            let b = lazy_all.handle_special_key(1, "");
            assert_eq!(a.action, "commit", "keys={keys}");
            assert_eq!(a.commit_text, b.commit_text, "keys={keys}");
        }
    }

    #[test]
    fn mobile_engine_types_locks_and_commits_with_real_model_if_present() {
        let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("models")
            .join("ime_model.bin");
        if !model_path.is_file() {
            return;
        }
        let reranker_path = model_path.with_file_name("reranker.bin");
        let (mut eng, load_ms) =
            MobileEngine::load(&model_path, Some(&reranker_path), None).expect("load model");
        assert!(load_ms >= 0.0);

        let mut last = eng.handle_char('c', false, "");
        for ch in "eshi".chars() {
            last = eng.handle_char(ch, false, "");
        }
        assert_eq!(last.action, "update");
        assert_eq!(last.panel_preedit, "ce'shi");
        assert!(last.candidates.iter().any(|c| c == "测试"));

        // Find single-char `测` and tap it to lock first segment, then tap `试`.
        let ce_idx = last
            .candidates
            .iter()
            .position(|c| c == "测")
            .expect("prefix char 测 should appear");
        let locked = eng.pick_candidate(ce_idx, "");
        assert_eq!(locked.action, "update");
        assert_eq!(locked.inline_preedit, "测shi");
        assert_eq!(locked.panel_preedit, "测 shi");

        let shi_idx = locked
            .candidates
            .iter()
            .position(|c| c == "试")
            .expect("试 should appear for remaining shi");
        let committed = eng.pick_candidate(shi_idx, "");
        assert_eq!(committed.action, "commit");
        assert_eq!(committed.commit_text.as_deref(), Some("测试"));
        assert!(!committed.composing);
    }
}
