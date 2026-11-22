//! Shared IPC wire protocol, Win32 Named Pipe transport, and IME composing session logic.

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
        let pipe_name = format!(r"\\.\pipe\InputEngineTSF_unit_test_{}", std::process::id());
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

        // 2. Typing mid-composition + tapping Shift switches candidates directly to English without committing.
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
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.mode(), InputMode::English);
        assert!(s.is_composing());
        assert!(s.is_english_composing());
        assert_eq!(s.keys(), "hel");
        let page = s.current_page().2;
        assert_eq!(page.first().map(String::as_str), Some("hel"));
        assert!(page.iter().any(|c| c == "hello"));

        // Continuing to type updates English candidates locally with zero IPC, Space commits English.
        assert_eq!(
            s.handle_key(KeyEvent::Char('l'), none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(
            s.handle_key(KeyEvent::Char('o'), none),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        assert_eq!(s.current_page().2.first().map(String::as_str), Some("hello"));

        // Tapping Shift again mid-composition switches back to Chinese and requests conversion.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(
            s.handle_key(KeyEvent::ShiftUp, none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert_eq!(s.mode(), InputMode::Chinese);
        assert!(!s.is_english_composing());

        // Chromium dropped-ShiftUp mid-composition also switches directly to English candidates.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(
            s.consume_dropped_shift_tap(KeyEvent::Space, none),
            Some(KeyAction::UpdateComposition {
                need_convert: false
            })
        );
        assert_eq!(s.mode(), InputMode::English);
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "hello".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        // Idle English mode passes keys through; tap Shift to return to Chinese.
        assert!(!s.should_consume(KeyEvent::Char('a'), none));
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::ConsumedNoChange);
        assert_eq!(s.mode(), InputMode::Chinese);

        // 3. CamelCase NiloToon in Chinese mode: 'N' passes through, 'ilo' composes, Shift+'T'+'oon' appends into composition.
        s.note_test_key(KeyEvent::ShiftDown, shift);
        assert!(!s.should_consume(KeyEvent::UpperChar('N'), shift));
        s.note_test_key(KeyEvent::UpperChar('N'), shift);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);
        for ch in ['i', 'l', 'o'] {
            assert!(s.should_consume(KeyEvent::Char(ch), none));
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition { need_convert: true }
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
        assert_eq!(s.keys(), "iloT");
        assert!(s.is_english_composing());
        assert_eq!(s.mode(), InputMode::Chinese);
        assert_eq!(s.handle_key(KeyEvent::ShiftUp, none), KeyAction::PassThrough);

        // Backspace off 'T' reverts immediately back to Chinese conversion.
        assert_eq!(
            s.handle_key(KeyEvent::Backspace, none),
            KeyAction::UpdateComposition { need_convert: true }
        );
        assert!(!s.is_english_composing());

        // Re-type 'Toon' then Space commits "iloToon" in order and keeps Chinese mode.
        assert_eq!(
            s.handle_key(KeyEvent::UpperChar('T'), shift),
            KeyAction::UpdateComposition {
                need_convert: false
            }
        );
        for ch in ['o', 'o', 'n'] {
            assert!(s.should_consume(KeyEvent::Char(ch), none));
            assert_eq!(
                s.handle_key(KeyEvent::Char(ch), none),
                KeyAction::UpdateComposition {
                    need_convert: false
                }
            );
        }
        assert_eq!(s.keys(), "iloToon");
        assert_eq!(
            s.handle_key(KeyEvent::Space, none),
            KeyAction::Commit {
                text: "iloToon".into(),
                learn_keys: None,
                learn_text: None,
            }
        );
        assert_eq!(s.mode(), InputMode::Chinese);

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

        // 5. Fallback helper commits open composition cleanly and returns None when idle.
        s.handle_key(KeyEvent::Char('w'), none);
        s.handle_key(KeyEvent::Char('o'), none);
        assert_eq!(s.commit_raw_for_passthrough(), Some("wo".into()));
        assert!(!s.is_composing());
        assert_eq!(s.commit_raw_for_passthrough(), None);
    }
}
