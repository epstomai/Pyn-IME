//! Shared IPC wire protocol, Win32 Named Pipe transport, and IME composing session logic.

pub mod config;
pub mod english;
pub mod pipe;
pub mod protocol;
pub mod session;

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::thread;

    use crate::pipe::{NamedPipeClient, NamedPipeServerInstance};
    use crate::protocol::{read_frame, write_frame, IpcRequest, IpcResponse, MAX_FRAME_BYTES};
    use crate::session::{InputMode, KeyAction, KeyEvent, KeyModifiers, SessionController};

    #[test]
    fn frame_roundtrip_and_bounds() {
        let req = IpcRequest::Convert {
            session_id: 42,
            context: "我已经吃过饭".into(),
            keys: "le".into(),
            limit: 10,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let mut cur = Cursor::new(buf);
        let decoded: IpcRequest = read_frame(&mut cur).unwrap();
        assert_eq!(decoded, req);

        // Oversized length prefix is rejected without OOM.
        let bad_len = ((MAX_FRAME_BYTES as u32) + 1).to_le_bytes();
        let mut bad_cur = Cursor::new(bad_len.to_vec());
        assert!(read_frame::<_, IpcRequest>(&mut bad_cur).is_err());
    }

    #[test]
    fn named_pipe_appcontainer_dacl_roundtrip() {
        let pipe_name = format!(r"\\.\pipe\PynTSF_unit_test_{}", std::process::id());
        let server = NamedPipeServerInstance::create(&pipe_name).unwrap();
        let pipe_name_srv = pipe_name.clone();
        let handle = thread::spawn(move || {
            server.accept().unwrap();
            let mut srv = server;
            let req: IpcRequest = read_frame(&mut srv).unwrap();
            assert_eq!(req, IpcRequest::Ping);
            let resp = IpcResponse::Pong {
                version: "0.1.0".into(),
                model_loaded: true,
                reranker_loaded: true,
                learning_enabled: true,
            };
            write_frame(&mut srv, &resp).unwrap();
            let req2: IpcRequest = read_frame(&mut srv).unwrap();
            assert_eq!(req2, IpcRequest::Shutdown);
            write_frame(&mut srv, &IpcResponse::Ack { ok: true }).unwrap();
            let _ = pipe_name_srv;
        });

        let mut client = NamedPipeClient::connect(&pipe_name, 1000).unwrap();
        let pong = client.request(&IpcRequest::Ping).unwrap();
        assert_eq!(
            pong,
            IpcResponse::Pong {
                version: "0.1.0".into(),
                model_loaded: true,
                reranker_loaded: true,
                learning_enabled: true,
            }
        );
        let ack = client.request(&IpcRequest::Shutdown).unwrap();
        assert_eq!(ack, IpcResponse::Ack { ok: true });
        handle.join().unwrap();
    }

    #[test]
    fn session_composing_paging_punctuation_shift_and_undo() {
        let mut s = SessionController::new();
        let none = KeyModifiers::default();
        let shift = KeyModifiers {
            shift: true,
            ..Default::default()
        };

        // Typing pinyin and selecting paged candidate.
        assert!(!s.should_consume(KeyEvent::Left, none));
        assert!(!s.should_consume(KeyEvent::Right, none));
        assert!(s.should_consume(KeyEvent::Char('w'), none));
        assert_eq!(
            s.handle_key(KeyEvent::Char('w'), none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert_eq!(
            s.handle_key(KeyEvent::Char('o'), none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        s.set_candidates(
            ["我", "喔", "窝", "握", "卧", "蜗", "沃"]
                .into_iter()
                .map(String::from)
                .collect(),
        );
        assert!(s.should_consume(KeyEvent::Left, none));
        assert!(s.should_consume(KeyEvent::Right, none));
        assert_eq!(
            s.handle_key(KeyEvent::Right, none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.current_page().3, 1);
        assert_eq!(
            s.handle_key(KeyEvent::Left, none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.current_page().3, 0);
        // Left wraps from index 0 to last candidate.
        assert_eq!(
            s.handle_key(KeyEvent::Left, none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.current_page(), (1, 2, &["蜗".into(), "沃".into()][..], 1));
        // Right wraps back to index 0.
        assert_eq!(
            s.handle_key(KeyEvent::Right, none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.current_page().3, 0);
        assert_eq!(s.current_page().1, 2);
        assert_eq!(
            s.handle_key(KeyEvent::Char('='), none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        let (page_idx, total_pages, slice, sel) = s.current_page();
        assert_eq!((page_idx, total_pages, slice, sel), (1, 2, &["蜗".into(), "沃".into()][..], 0));
        assert_eq!(
            s.handle_key(KeyEvent::Digit(2), none),
            KeyAction::Commit {
                text: "沃".into(),
                learn_keys: Some("wo".into()),
                learn_text: Some("沃".into()),
            }
        );

        // Post-commit Backspace triggers undo once, then passes through normally.
        assert!(!s.should_consume(KeyEvent::Backspace, none));
        assert!(s.should_undo_on_passthrough_key(KeyEvent::Backspace, none));
        assert!(!s.should_undo_on_passthrough_key(KeyEvent::Backspace, none));

        // Punctuation auto-commit separates `learn_text` from trailing Chinese punctuation.
        s.handle_key(KeyEvent::Char('w'), none);
        s.handle_key(KeyEvent::Char('o'), none);
        s.set_candidates(vec!["我".into()]);
        assert_eq!(
            s.handle_key(KeyEvent::Char(','), none),
            KeyAction::Commit {
                text: "我，".into(),
                learn_keys: Some("wo".into()),
                learn_text: Some("我".into()),
            }
        );

        // Paired quotes alternate open/close cleanly.
        assert_eq!(
            s.handle_key(KeyEvent::Char('"'), shift),
            KeyAction::Commit {
                text: "“".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert_eq!(
            s.handle_key(KeyEvent::Char('"'), shift),
            KeyAction::Commit {
                text: "”".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        // Shift+A must NOT toggle mode, while clean Shift tap toggles Chinese <-> English.
        assert_eq!(s.mode(), InputMode::Chinese);
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert!(!s.should_consume(KeyEvent::Char('a'), shift));
        s.note_test_key(KeyEvent::Char('a'), shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);
        assert_eq!(s.mode(), InputMode::Chinese);

        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::ConsumedNoChange);
        assert_eq!(s.mode(), InputMode::English);
        assert!(!s.should_consume(KeyEvent::Char('a'), none));

        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::ConsumedNoChange);
        assert_eq!(s.mode(), InputMode::Chinese);
    }

    #[test]
    fn session_mid_typing_shift_switch_camelcase_and_edge_keys() {
        let mut s = SessionController::new();
        let none = KeyModifiers::default();
        let shift = KeyModifiers {
            shift: true,
            ..Default::default()
        };

        // 1. Long pinyin or typo never auto-commits or locks into temp English; Backspace stays Chinese.
        for ch in "woxiangqugongyuxn".chars() {
            assert!(s.should_consume(KeyEvent::Char(ch), none));
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        assert!(s.is_composing());
        assert!(!s.is_english_composing());
        assert_eq!(s.mode(), InputMode::Chinese);
        assert_eq!(
            s.handle_key(KeyEvent::Backspace, none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert_eq!(s.handle_key(KeyEvent::Escape, none), KeyAction::CancelComposition);

        // Even "git" starts normal Chinese composing instead of auto-committing.
        for ch in ['g', 'i', 't'] {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        assert!(s.is_composing());
        assert_eq!(s.mode(), InputMode::Chinese);
        s.handle_key(KeyEvent::Escape, none);

        // 2. Typing mid-composition in Chinese mode + tapping Shift immediately commits raw keys and switches to English.
        for ch in ['h', 'e', 'l'] {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        s.set_candidates(vec!["喝了".into(), "和了".into()]);

        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(
            s.handle_key(KeyEvent::ShiftUp, none),
            KeyAction::Commit {
                text: "hel".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert_eq!(s.mode(), InputMode::English);
        assert!(!s.is_composing());
        assert_eq!(s.keys(), "");
        // Subsequent English keys pass straight through to the application.
        assert!(!s.should_consume(KeyEvent::Char('l'), none));
        assert!(!s.should_consume(KeyEvent::Char('o'), none));

        // Tap Shift while idle to return to Chinese.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::ConsumedNoChange);
        assert_eq!(s.mode(), InputMode::Chinese);

        // Chromium dropped-ShiftUp mid-composition also immediately commits raw keys and switches to English,
        // allowing the next key to pass straight through into the input box.
        for ch in ['h', 'e', 'l', 'l', 'o'] {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(
            s.consume_dropped_shift_tap(KeyEvent::Space, none),
            Some(KeyAction::Commit {
                text: "hello".into(),
                learn_keys: None,
                learn_text: None,
            })
        );
        assert_eq!(s.mode(), InputMode::English);
        assert!(!s.is_composing());
        assert!(!s.should_consume(KeyEvent::Space, none));
        assert!(!s.should_consume(KeyEvent::Char('a'), none));

        // Tap Shift to return to Chinese.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::ConsumedNoChange);
        assert_eq!(s.mode(), InputMode::Chinese);

        // 3. Initial uppercase CamelCase `NiloToon` in Chinese mode enters IME from the first letter 'N'.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert!(s.should_consume(KeyEvent::UpperChar('N'), shift));
        s.note_test_key(KeyEvent::UpperChar('N'), shift);
        assert_eq!(
            s.handle_key(KeyEvent::UpperChar('N'), shift),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert!(s.is_composing());
        assert!(s.is_english_composing());
        assert_eq!(s.keys(), "N");
        assert_eq!(s.current_page().2, &["N".to_owned()][..]);
        assert_eq!(s.mode(), InputMode::Chinese);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);
        assert_eq!(s.mode(), InputMode::Chinese);

        // Backspace on single uppercase cancels composition cleanly.
        assert_eq!(
            s.handle_key(KeyEvent::Backspace, none),
            KeyAction::CancelComposition
        );
        assert!(!s.is_composing());

        // Type full `NiloToon`: stays inside composition from 'N' through 'n', Space commits "NiloToon" in Chinese mode.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert!(s.should_consume(KeyEvent::UpperChar('N'), shift));
        s.note_test_key(KeyEvent::UpperChar('N'), shift);
        assert_eq!(
            s.handle_key(KeyEvent::UpperChar('N'), shift),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);
        for ch in ['i', 'l', 'o'] {
            assert!(s.should_consume(KeyEvent::Char(ch), none));
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition {
                    need_convert: false
                }
            );
        }
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert!(s.should_consume(KeyEvent::UpperChar('T'), shift));
        s.note_test_key(KeyEvent::UpperChar('T'), shift);
        assert_eq!(
            s.handle_key(KeyEvent::UpperChar('T'), shift),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.keys(), "NiloT");
        assert!(s.is_english_composing());
        assert_eq!(s.mode(), InputMode::Chinese);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);

        for ch in ['o', 'o', 'n'] {
            assert!(s.should_consume(KeyEvent::Char(ch), none));
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition {
                    need_convert: false
                }
            );
        }
        assert_eq!(s.keys(), "NiloToon");
        assert_eq!(s.current_page().2, &["NiloToon".to_owned()][..]);
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "NiloToon".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert_eq!(s.mode(), InputMode::Chinese);

        // 3b. Uppercase-started English composing in Chinese mode + solo Shift tap also immediately commits raw text and switches to English.
        s.handle_key(KeyEvent::UpperChar('N'), shift);
        for ch in ['i', 'h', 'a', 'o'] {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        assert!(s.is_english_composing());
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(
            s.handle_key(KeyEvent::ShiftUp, none),
            KeyAction::Commit {
                text: "Nihao".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert_eq!(s.mode(), InputMode::English);
        assert!(!s.is_composing());
        assert_eq!(s.keys(), "");
        // Return to Chinese mode for the remaining checks.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::ConsumedNoChange);
        assert_eq!(s.mode(), InputMode::Chinese);

        // 3c. Mid-word Shift+T during lowercase Chinese composing also keeps English composing, Backspace reverts.
        for ch in ['i', 'l', 'o'] {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        assert_eq!(
            s.handle_key(KeyEvent::UpperChar('T'), shift),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert!(s.is_english_composing());
        assert_eq!(
            s.handle_key(KeyEvent::Backspace, none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert!(!s.is_english_composing());
        s.handle_key(KeyEvent::Escape, none);

        // 3d. CapsLock (`RawChar`) passes through directly when idle and commits cleanly when composing.
        assert!(!s.should_consume(KeyEvent::RawChar('A'), none));
        assert_eq!(s.handle_key(KeyEvent::RawChar('A'), none), KeyAction::PassThrough);
        assert!(!s.is_composing());

        // 4. Composing + Digit(0) / RawChar('+') / Char('/') commit cleanly without locking English.
        for (ev, m, expected) in [
            (KeyEvent::Digit(0), none, "nilo0"),
            (KeyEvent::RawChar('+'), shift, "nilo+"),
            (KeyEvent::Char('/'), none, "nilo/"),
        ] {
            for ch in ['n', 'i', 'l', 'o'] {
                s.handle_key(KeyEvent::Char(ch), none);
            }
            assert!(s.should_consume(ev, m));
            assert_eq!(
                s.handle_key(ev, m),
                KeyAction::Commit {
                    text: expected.into(),
                    learn_keys: None,
                    learn_text: None,
                }
            );
            assert!(!s.is_composing());
            assert!(s.should_consume(KeyEvent::Char('w'), none));
            s.handle_key(KeyEvent::Escape, none);
        }

        // 4b. English-only candidates in Chinese mode (e.g. `Unity` + `6` / `2` / `1`, or lowercase
        // non-pinyin `unity` + `6`) commit `raw + digit` directly into the input box while staying
        // in Chinese mode; subsequent digits pass straight through (`Unity2022`).
        for (digit, expected) in [(6, "Unity6"), (2, "Unity2"), (1, "Unity1")] {
            s.handle_key(KeyEvent::UpperChar('U'), shift);
            for ch in ['n', 'i', 't', 'y'] {
                s.handle_key(KeyEvent::Char(ch), none);
            }
            assert_eq!(s.current_page().2, &["Unity".to_owned()][..]);
            assert_eq!(
                s.handle_key(KeyEvent::Digit(digit), none),
                KeyAction::Commit {
                    text: expected.into(),
                    learn_keys: None,
                    learn_text: None,
                }
            );
            assert!(!s.is_composing());
            assert_eq!(s.mode(), InputMode::Chinese);
            assert!(!s.should_consume(KeyEvent::Digit(0), none));
        }

        // Lowercase non-pinyin with empty/ASCII-only candidates also commits `unity6`.
        for ch in ['u', 'n', 'i', 't', 'y'] {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        s.set_candidates(vec![]);
        assert_eq!(
            s.handle_key(KeyEvent::Digit(6), none),
            KeyAction::Commit {
                text: "unity6".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert!(!s.is_composing());

        // Multi-candidate English completions (`Hel` -> `["Hel", "Hello", "Help"]`):
        // digits within candidate range pick completion, out-of-range digits commit `Hel6`.
        s.handle_key(KeyEvent::UpperChar('H'), shift);
        s.handle_key(KeyEvent::Char('e'), none);
        s.handle_key(KeyEvent::Char('l'), none);
        assert!(s.current_page().2.len() >= 2);
        assert_eq!(
            s.handle_key(KeyEvent::Digit(6), none),
            KeyAction::Commit {
                text: "Hel6".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert!(!s.is_composing());

        // Chinese candidates must NOT commit raw pinyin when pressing an out-of-range digit.
        s.handle_key(KeyEvent::Char('w'), none);
        s.handle_key(KeyEvent::Char('o'), none);
        s.set_candidates(vec!["我".into(), "喔".into()]);
        assert_eq!(
            s.handle_key(KeyEvent::Digit(6), none),
            KeyAction::ConsumedNoChange
        );
        assert!(s.is_composing());
        s.handle_key(KeyEvent::Escape, none);

        // 5. Fallback helper commits open composition cleanly and returns None when idle.
        s.handle_key(KeyEvent::Char('w'), none);
        s.handle_key(KeyEvent::Char('o'), none);
        assert_eq!(s.commit_raw_for_passthrough(), Some("wo".into()));
        assert!(!s.is_composing());
        assert_eq!(s.commit_raw_for_passthrough(), None);
    }

    #[test]
    fn preserved_shift_tap_and_english_punctuation_toggle() {
        let mut s = SessionController::new();
        let none = KeyModifiers::default();
        let shift = KeyModifiers {
            shift: true,
            ..Default::default()
        };
        let ctrl = KeyModifiers {
            ctrl: true,
            ..Default::default()
        };
        let ctrl_shift = KeyModifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };

        // 1. Preserved Shift tap toggles Chinese <-> English even if host skipped OnTestKeyDown(VK_SHIFT).
        assert_eq!(s.mode(), InputMode::Chinese);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::English);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::Chinese);

        // With explicit ShiftDown first also toggles cleanly without double-toggling.
        s.note_test_key_with_repeat(KeyEvent::ShiftDown, shift, false);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::English);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);
        assert_eq!(s.mode(), InputMode::English);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::Chinese);

        // 2. Shift+N + repeated ShiftDown hold + release MUST NOT toggle mode.
        s.note_test_key_with_repeat(KeyEvent::ShiftDown, shift, false);
        s.note_test_key_with_repeat(KeyEvent::UpperChar('N'), shift, false);
        s.handle_key(KeyEvent::UpperChar('N'), shift);
        s.note_test_key_with_repeat(KeyEvent::ShiftDown, shift, true);
        assert_eq!(s.take_preserved_shift_tap(), None);
        assert_eq!(s.mode(), InputMode::Chinese);
        s.handle_key(KeyEvent::Escape, none);

        // 3. Shift+( symbol in English/Chinese mode marks combo_used and does NOT toggle.
        s.note_test_key_with_repeat(KeyEvent::ShiftDown, shift, false);
        s.note_test_key_with_repeat(KeyEvent::Char('('), shift, false);
        assert_eq!(s.take_preserved_shift_tap(), None);
        assert_eq!(s.mode(), InputMode::Chinese);
        // Subsequent solo Shift tap DOES toggle.
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::English);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::Chinese);

        // Ctrl+Shift combo rejects preserved tap.
        s.note_test_key_with_repeat(KeyEvent::Other, ctrl_shift, false);
        assert_eq!(s.take_preserved_shift_tap(), None);
        assert_eq!(s.mode(), InputMode::Chinese);

        // Plain Ctrl+C (no Shift) must NOT swallow a subsequent solo Shift tap.
        s.note_test_key_with_repeat(KeyEvent::Char('c'), ctrl, false);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::English);
        assert_eq!(s.take_preserved_shift_tap(), Some(KeyAction::ConsumedNoChange));
        assert_eq!(s.mode(), InputMode::Chinese);

        // 4. Mid-composition preserved Shift tap in Chinese mode immediately commits raw keys and switches to English.
        for ch in ['h', 'e', 'l'] {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        s.set_candidates(vec!["喝了".into()]);
        assert_eq!(
            s.take_preserved_shift_tap(),
            Some(KeyAction::Commit {
                text: "hel".into(),
                learn_keys: None,
                learn_text: None,
            })
        );
        assert_eq!(s.mode(), InputMode::English);
        assert!(!s.is_composing());
        assert_eq!(s.keys(), "");
        assert_eq!(
            s.take_preserved_shift_tap(),
            Some(KeyAction::ConsumedNoChange)
        );
        assert_eq!(s.mode(), InputMode::Chinese);

        // 5. Toggle English punctuation while staying in Chinese mode.
        assert!(s.chinese_punct());
        assert!(!s.toggle_chinese_punct());
        assert!(!s.should_consume(KeyEvent::Char(','), none));
        assert!(!s.should_consume(KeyEvent::Char('('), shift));
        assert!(s.should_consume(KeyEvent::Char('w'), none));

        // Composing Chinese + comma with English punctuation commits half-width comma in order.
        s.handle_key(KeyEvent::Char('w'), none);
        s.handle_key(KeyEvent::Char('o'), none);
        s.set_candidates(vec!["我".into()]);
        assert_eq!(
            s.handle_key(KeyEvent::Char(','), none),
            KeyAction::Commit {
                text: "我,".into(),
                learn_keys: Some("wo".into()),
                learn_text: Some("我".into()),
            }
        );

        assert!(s.toggle_chinese_punct());
        assert!(s.should_consume(KeyEvent::Char(','), none));
    }

    #[test]
    fn config_persistence_normalization_and_session_apply() {
        use crate::config::{CandidateLayout, HighlightStyle, ImeConfig, ThemeMode};

        let raw = ImeConfig {
            theme: ThemeMode::Dark,
            layout: CandidateLayout::Vertical,
            highlight_style: HighlightStyle::Inverted,
            font_family: "  MiSans  ".into(),
            font_size: 40,
            selected_weight: 490,
            page_size: 20,
            show_preedit_caret: false,
            show_mode_toast: false,
            default_chinese_punct: false,
            smart_english_punct: false,
            underscore_halfwidth: false,
            shift_toggle: false,
            initial_upper_english: false,
            learning_enabled: false,
            cs_ai_boost: false,
        };
        let norm = raw.normalized();
        assert_eq!(norm.font_family, "MiSans");
        assert_eq!(norm.font_size, 22);
        assert_eq!(norm.selected_weight, 500);
        assert_eq!(norm.page_size, 9);

        let dir = std::env::temp_dir().join(format!("ime_cfg_test_{}", std::process::id()));
        let path = dir.join("config.json");
        norm.save_atomic(&path).unwrap();
        let loaded = ImeConfig::load_from(&path);
        assert_eq!(loaded, norm);
        let _ = std::fs::remove_dir_all(&dir);

        // Wire frame roundtrip for GetConfig / SetConfig.
        let req = IpcRequest::SetConfig { config: norm.clone() };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let mut cur = Cursor::new(buf);
        let dec: IpcRequest = read_frame(&mut cur).unwrap();
        assert_eq!(dec, req);

        // SessionController applies page_size, shift_toggle, initial_upper_english,
        // default_chinese_punct, smart_english_punct, underscore_halfwidth.
        let mut s = SessionController::new();
        s.apply_config(&norm, None);
        assert_eq!(s.page_size(), 9);
        assert!(!s.shift_toggle_enabled());
        assert!(!s.initial_upper_english());
        assert!(!s.chinese_punct());
        assert!(!s.smart_english_punct());
        assert!(!s.underscore_halfwidth());
        assert_eq!(s.take_preserved_shift_tap(), None);
        assert!(!s.should_consume(KeyEvent::UpperChar('N'), KeyModifiers { shift: true, ..Default::default() }));

        // Updating another setting preserves ad-hoc Ctrl+. punctuation state until default_chinese_punct changes.
        assert!(s.toggle_chinese_punct());
        let mut updated = norm.clone();
        updated.font_size = 16;
        s.apply_config(&updated, Some(&norm));
        assert!(s.chinese_punct());
    }

    #[test]
    fn session_segment_locking_brackets_tab_and_wire_compat() {
        use crate::protocol::{WireCandidate, WireCandidateKind};

        // 1. Backward-compatible JSON deserialization when new rich fields are omitted.
        let legacy_json = r#"{"kind":"candidates","candidates":["家里床","嘉"],"latency_ms":1.1}"#;
        let parsed: IpcResponse = serde_json::from_str(legacy_json).unwrap();
        match parsed {
            IpcResponse::Candidates {
                candidates,
                items,
                segmented_preedit,
                first_char_offset,
                first_syllable_len,
                ..
            } => {
                assert_eq!(candidates, vec!["家里床", "嘉"]);
                assert!(items.is_empty());
                assert_eq!(segmented_preedit, None);
                assert_eq!(first_char_offset, None);
                assert_eq!(first_syllable_len, 0);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        // 2. Segment locking flow: `jialichuang` -> pick `嘉` (3) -> `lichuang` -> pick `立创` (8) -> Commit `嘉立创`.
        let mut s = SessionController::new();
        let none = KeyModifiers::default();
        let shift = KeyModifiers {
            shift: true,
            ..Default::default()
        };
        for ch in "jialichuang".chars() {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "家里床".into(), consumed_len: 11, kind: WireCandidateKind::Full },
                WireCandidate { text: "家里创".into(), consumed_len: 11, kind: WireCandidateKind::Full },
                WireCandidate { text: "家里".into(), consumed_len: 5, kind: WireCandidateKind::PrefixWord },
                WireCandidate { text: "家".into(), consumed_len: 3, kind: WireCandidateKind::PrefixChar },
                WireCandidate { text: "嘉".into(), consumed_len: 3, kind: WireCandidateKind::PrefixChar },
            ],
            Some("jia'li'chuang".into()),
            Some(3),
            3,
        );
        assert_eq!(s.panel_preedit(), "jia'li'chuang");
        assert!(!s.is_single_char_mode());

        // Tab enters single-char quick view (index 0 is `家`, index 1 is `嘉`).
        assert_eq!(
            s.handle_key(KeyEvent::Tab, none),
            KeyAction::UpdateComposition { need_convert: false }
        );
        assert!(s.is_single_char_mode());
        assert_eq!(s.current_page().2, &["家".to_owned(), "嘉".to_owned()][..]);

        // Tab again restores original view and selection.
        assert_eq!(
            s.handle_key(KeyEvent::Tab, none),
            KeyAction::UpdateComposition { need_convert: false }
        );
        assert!(!s.is_single_char_mode());
        assert_eq!(s.current_page().2.first().map(String::as_str), Some("家里床"));

        // Enter single-char view and select `嘉` via Digit(2).
        s.handle_key(KeyEvent::Tab, none);
        assert_eq!(
            s.handle_key(KeyEvent::Digit(2), none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert!(s.has_locked());
        assert_eq!(s.locked_text(), "嘉");
        assert_eq!(s.keys(), "lichuang");
        assert_eq!(s.inline_preedit(), "嘉lichuang");
        assert!(!s.is_single_char_mode());

        // Backspace while locked pops `嘉` and restores `jialichuang`.
        assert_eq!(
            s.handle_key(KeyEvent::Backspace, none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert!(!s.has_locked());
        assert_eq!(s.keys(), "jialichuang");

        // Re-apply candidates and test `[` / `]` bracket pick.
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "嘉利创".into(), consumed_len: 11, kind: WireCandidateKind::Full },
                WireCandidate { text: "家里".into(), consumed_len: 5, kind: WireCandidateKind::PrefixWord },
                WireCandidate { text: "嘉".into(), consumed_len: 3, kind: WireCandidateKind::PrefixChar },
            ],
            Some("jia'li'chuang".into()),
            Some(2),
            3,
        );
        // `[` picks first char `嘉` (3 bytes) and keeps `lichuang`.
        assert_eq!(
            s.handle_key(KeyEvent::RawChar('['), none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert_eq!(s.locked_text(), "嘉");
        assert_eq!(s.keys(), "lichuang");

        // Now complete with `立创`.
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "立创".into(), consumed_len: 8, kind: WireCandidateKind::Full },
                WireCandidate { text: "立".into(), consumed_len: 2, kind: WireCandidateKind::PrefixChar },
            ],
            Some("li'chuang".into()),
            Some(1),
            2,
        );
        assert_eq!(s.panel_preedit(), "嘉 li'chuang");
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "嘉立创".into(),
                learn_keys: Some("jialichuang".into()),
                learn_text: Some("嘉立创".into()),
            }
        );
        assert!(!s.is_composing());

        // 3. `]` picks last char of highlighted word; punctuation while prefix highlighted does not drop trailing keys.
        for ch in "jialichuang".chars() {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "家里床".into(), consumed_len: 11, kind: WireCandidateKind::Full },
                WireCandidate { text: "佳丽".into(), consumed_len: 5, kind: WireCandidateKind::PrefixWord },
            ],
            Some("jia'li'chuang".into()),
            None,
            3,
        );
        // Move highlight to prefix `佳丽` (index 1) and press `]` -> locks `丽` (5 bytes), leaves `chuang`.
        s.handle_key(KeyEvent::Down, none);
        assert_eq!(
            s.handle_key(KeyEvent::RawChar(']'), none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert_eq!(s.locked_text(), "丽");
        assert_eq!(s.keys(), "chuang");

        // Pressing comma while a prefix candidate is highlighted promotes the full-span candidate (`创`).
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "创".into(), consumed_len: 6, kind: WireCandidateKind::Full },
                WireCandidate { text: "ch".into(), consumed_len: 2, kind: WireCandidateKind::PrefixChar },
            ],
            Some("chuang".into()),
            Some(1),
            6,
        );
        s.handle_key(KeyEvent::Down, none);
        assert_eq!(
            s.handle_key(KeyEvent::Char(','), none),
            KeyAction::Commit {
                text: "丽创，".into(),
                learn_keys: Some("jialichuang".into()),
                learn_text: Some("丽创".into()),
            }
        );

        // 4. English composing passes `[` through literally instead of triggering first-char pick.
        s.handle_key(KeyEvent::UpperChar('A'), shift);
        assert_eq!(
            s.handle_key(KeyEvent::RawChar('['), none),
            KeyAction::Commit {
                text: "A[".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        // 5. 2-syllable `ceshi` with page_size=6: picking #4 `测` (consumed_len=2) locks `测` and keeps `shi`.
        s.set_page_size(6);
        for ch in "ceshi".chars() {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "测试".into(), consumed_len: 5, kind: WireCandidateKind::Full },
                WireCandidate { text: "策士".into(), consumed_len: 5, kind: WireCandidateKind::Full },
                WireCandidate { text: "测时".into(), consumed_len: 5, kind: WireCandidateKind::Full },
                WireCandidate { text: "测".into(), consumed_len: 2, kind: WireCandidateKind::PrefixChar },
                WireCandidate { text: "侧".into(), consumed_len: 2, kind: WireCandidateKind::PrefixChar },
                WireCandidate { text: "策".into(), consumed_len: 2, kind: WireCandidateKind::PrefixChar },
            ],
            Some("ce'shi".into()),
            Some(3),
            2,
        );
        assert_eq!(
            s.handle_key(KeyEvent::Digit(4), none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert!(s.has_locked());
        assert_eq!(s.locked_text(), "测");
        assert_eq!(s.keys(), "shi");
        assert_eq!(s.inline_preedit(), "测shi");
        s.set_rich_candidates(
            vec![
                WireCandidate { text: "试".into(), consumed_len: 3, kind: WireCandidateKind::Full },
                WireCandidate { text: "时".into(), consumed_len: 3, kind: WireCandidateKind::Full },
            ],
            Some("shi".into()),
            Some(0),
            3,
        );
        assert_eq!(
            s.handle_key(KeyEvent::Digit(1), none),
            KeyAction::Commit {
                text: "测试".into(),
                learn_keys: Some("ceshi".into()),
                learn_text: Some("测试".into()),
            }
        );
        assert!(!s.is_composing());
    }

    #[test]
    fn smart_english_punctuation_underscore_and_seamless_identifier_typing() {
        use crate::protocol::{WireCandidate, WireCandidateKind};

        let mut s = SessionController::new();
        let none = KeyModifiers::default();
        let shift = KeyModifiers {
            shift: true,
            ..Default::default()
        };

        // 1. `_` defaults to halfwidth `_` even after Chinese text or in idle Chinese mode.
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('_'), shift, Some('中')),
            KeyAction::Commit {
                text: "_".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        // Disabling `underscore_halfwidth` emits `——` after Chinese, while `smart_english_punct` still protects English/digits.
        s.set_underscore_halfwidth(false);
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('_'), shift, Some('中')),
            KeyAction::Commit {
                text: "——".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('_'), shift, Some('a')),
            KeyAction::Commit {
                text: "_".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        s.set_underscore_halfwidth(true);

        // 2. Context-aware `.`: halfwidth at start of line, after digit/alpha/path/space; fullwidth `。` after Hanzi / Chinese closing punct.
        for prev in [None, Some('3'), Some('n'), Some('/'), Some('.'), Some('_'), Some(' ')] {
            assert_eq!(
                s.handle_key_with_context(KeyEvent::Char('.'), none, prev),
                KeyAction::Commit {
                    text: ".".into(),
                    learn_keys: None,
                    learn_text: None,
                },
                "expected '.' after {prev:?}"
            );
        }
        for prev in [Some('好'), Some('）'), Some('”')] {
            assert_eq!(
                s.handle_key_with_context(KeyEvent::Char('.'), none, prev),
                KeyAction::Commit {
                    text: "。".into(),
                    learn_keys: None,
                    learn_text: None,
                },
                "expected '。' after {prev:?}"
            );
        }

        // 3. Context-aware `:`, `^`, `<`, `>`, and `'`: halfwidth after ASCII identifiers/digits, fullwidth after Chinese.
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char(':'), shift, Some('2')),
            KeyAction::Commit { text: ":".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char(':'), shift, Some('说')),
            KeyAction::Commit { text: "：".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('^'), shift, Some('2')),
            KeyAction::Commit { text: "^".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('^'), shift, Some('恩')),
            KeyAction::Commit { text: "……".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('<'), shift, Some('c')),
            KeyAction::Commit { text: "<".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('>'), shift, Some('T')),
            KeyAction::Commit { text: ">".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('<'), shift, Some('读')),
            KeyAction::Commit { text: "《".into(), learn_keys: None, learn_text: None }
        );

        // `don't` emits ASCII `'` without flipping paired Chinese single quotes `‘’`.
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('\''), none, Some('n')),
            KeyAction::Commit { text: "'".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('\''), none, Some('说')),
            KeyAction::Commit { text: "‘".into(), learn_keys: None, learn_text: None }
        );
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('\''), none, Some('他')),
            KeyAction::Commit { text: "’".into(), learn_keys: None, learn_text: None }
        );

        // Disabling `smart_english_punct` reverts `.` / `:` / `'` after ASCII back to fullwidth.
        s.set_smart_english_punct(false);
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('.'), none, Some('3')),
            KeyAction::Commit { text: "。".into(), learn_keys: None, learn_text: None }
        );
        s.set_smart_english_punct(true);

        // 4. Fallback tracking when host returns None for context (`GetText` unavailable).
        s.on_focus_change();
        s.note_passthrough_key(KeyEvent::Digit(3), none);
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('.'), none, None),
            KeyAction::Commit { text: ".".into(), learn_keys: None, learn_text: None }
        );
        s.handle_key(KeyEvent::Char('w'), none);
        s.handle_key(KeyEvent::Char('o'), none);
        s.set_candidates(vec!["我".into()]);
        s.handle_key(KeyEvent::Space, none);
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('.'), none, None),
            KeyAction::Commit { text: "。".into(), learn_keys: None, learn_text: None }
        );
        s.note_passthrough_key(KeyEvent::Enter, none);
        assert_eq!(
            s.handle_key_with_context(KeyEvent::Char('.'), none, None),
            KeyAction::Commit { text: ".".into(), learn_keys: None, learn_text: None }
        );

        // 5. Seamless identifier typing inside uppercase English composing (`Input_Engine`, `README.md`, `Pyn-IME`).
        for (ev, m) in [
            (KeyEvent::UpperChar('I'), shift),
            (KeyEvent::Char('n'), none),
            (KeyEvent::Char('p'), none),
            (KeyEvent::Char('u'), none),
            (KeyEvent::Char('t'), none),
            (KeyEvent::Char('_'), shift),
            (KeyEvent::UpperChar('E'), shift),
            (KeyEvent::Char('n'), none),
            (KeyEvent::Char('g'), none),
            (KeyEvent::Char('i'), none),
            (KeyEvent::Char('n'), none),
            (KeyEvent::Char('e'), none),
        ] {
            assert_eq!(
                s.handle_key(ev, m),
                KeyAction::UpdateComposition { need_convert: false }
            );
        }
        assert_eq!(s.keys(), "Input_Engine");
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "Input_Engine".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        for (ev, m) in [
            (KeyEvent::UpperChar('R'), shift),
            (KeyEvent::UpperChar('E'), shift),
            (KeyEvent::UpperChar('A'), shift),
            (KeyEvent::UpperChar('D'), shift),
            (KeyEvent::UpperChar('M'), shift),
            (KeyEvent::UpperChar('E'), shift),
            (KeyEvent::Char('.'), none),
            (KeyEvent::Char('m'), none),
            (KeyEvent::Char('d'), none),
        ] {
            s.handle_key(ev, m);
        }
        assert_eq!(s.keys(), "README.md");
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "README.md".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        for (ev, m) in [
            (KeyEvent::UpperChar('P'), shift),
            (KeyEvent::Char('y'), none),
            (KeyEvent::Char('n'), none),
            (KeyEvent::Char('-'), none),
            (KeyEvent::UpperChar('I'), shift),
            (KeyEvent::UpperChar('M'), shift),
            (KeyEvent::UpperChar('E'), shift),
        ] {
            s.handle_key(ev, m);
        }
        assert_eq!(s.keys(), "Pyn-IME");
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "Pyn-IME".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        // 6. Lowercase Chinese composing + `_` transitions to English (`user_dict`), Backspace reverts to Chinese.
        for ch in "user".chars() {
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
            );
        }
        assert_eq!(
            s.handle_key(KeyEvent::Char('_'), shift),
            KeyAction::UpdateComposition { need_convert: false }
        );
        assert!(s.is_english_composing());
        assert_eq!(s.keys(), "user_");
        // Backspace back to `user` resumes Chinese decoding.
        assert_eq!(
            s.handle_key(KeyEvent::Backspace, none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert!(!s.is_english_composing());
        assert_eq!(
            s.handle_key(KeyEvent::Char('_'), shift),
            KeyAction::UpdateComposition { need_convert: false }
        );
        for ch in "dict".chars() {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "user_dict".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        // 7. Lowercase non-pinyin (`config` with only prefix candidates, no full-span Chinese) + `.` -> `config.json`.
        for ch in "config".chars() {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        s.set_rich_candidates(
            vec![WireCandidate {
                text: "吃哦".into(),
                consumed_len: 2,
                kind: WireCandidateKind::PrefixWord,
            }],
            Some("c'o'n'f'i'g".into()),
            None,
            1,
        );
        assert_eq!(
            s.handle_key(KeyEvent::Char('.'), none),
            KeyAction::UpdateComposition { need_convert: false }
        );
        for ch in "json".chars() {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "config.json".into(),
                learn_keys: None,
                learn_text: None,
            }
        );

        // Whereas valid full-span pinyin (`nihao` -> `你好`) + `.` still commits `你好。`.
        for ch in "nihao".chars() {
            s.handle_key(KeyEvent::Char(ch), none);
        }
        s.set_rich_candidates(
            vec![WireCandidate {
                text: "你好".into(),
                consumed_len: 5,
                kind: WireCandidateKind::Full,
            }],
            Some("ni'hao".into()),
            None,
            2,
        );
        assert_eq!(
            s.handle_key(KeyEvent::Char('.'), none),
            KeyAction::Commit {
                text: "你好。".into(),
                learn_keys: Some("nihao".into()),
                learn_text: Some("你好".into()),
            }
        );
    }
}
