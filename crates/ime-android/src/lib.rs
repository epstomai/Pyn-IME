//! Android JNI bindings for Pyn IME (`libpyn_ime.so`).

pub mod engine;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;

use crate::engine::{ActionPayload, InitPayload, MobileEngine, StatsPayload};

static ENGINE: OnceLock<Mutex<Option<MobileEngine>>> = OnceLock::new();

fn engine_slot() -> &'static Mutex<Option<MobileEngine>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

fn jstr_to_rust(env: &mut JNIEnv, s: JString) -> String {
    if s.is_null() {
        return String::new();
    }
    env.get_string(&s)
        .map(|js| js.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn to_jstring(env: &mut JNIEnv, text: &str) -> jstring {
    env.new_string(text)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn fallback_action_json(mode: &'static str) -> String {
    serde_json::to_string(&ActionPayload {
        action: "pass_through",
        commit_text: None,
        undo_previous: false,
        composing: false,
        mode,
        inline_preedit: String::new(),
        panel_preedit: String::new(),
        candidates: Vec::new(),
        single_char_mode: false,
        latency_ms: 0.0,
    })
    .unwrap_or_else(|_| r#"{"action":"pass_through"}"#.to_owned())
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    model_path: JString,
    reranker_path: JString,
    user_dict_path: JString,
) -> jstring {
    let model = PathBuf::from(jstr_to_rust(&mut env, model_path));
    let reranker_str = jstr_to_rust(&mut env, reranker_path);
    let user_dict_str = jstr_to_rust(&mut env, user_dict_path);
    let reranker = (!reranker_str.is_empty()).then(|| PathBuf::from(reranker_str));
    let user_dict = (!user_dict_str.is_empty()).then(|| PathBuf::from(user_dict_str));

    // Reuse already-loaded engine if the service restarts in the same process.
    if let Ok(guard) = engine_slot().lock() {
        if let Some(ref existing) = *guard {
            let resp = InitPayload {
                ok: true,
                load_ms: 0.0,
                reranker_loaded: existing.has_reranker(),
                error: None,
            };
            let json = serde_json::to_string(&resp).unwrap_or_default();
            return to_jstring(&mut env, &json);
        }
    }

    let resp = match MobileEngine::load(&model, reranker.as_deref(), user_dict.as_deref()) {
        Ok((eng, load_ms)) => {
            let reranker_loaded = eng.has_reranker();
            if let Ok(mut guard) = engine_slot().lock() {
                *guard = Some(eng);
            }
            InitPayload {
                ok: true,
                load_ms,
                reranker_loaded,
                error: None,
            }
        }
        Err(err) => InitPayload {
            ok: false,
            load_ms: 0.0,
            reranker_loaded: false,
            error: Some(err),
        },
    };

    let json = serde_json::to_string(&resp).unwrap_or_default();
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeIsReady(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    let ready = engine_slot()
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);
    if ready {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeSetSettings(
    _env: JNIEnv,
    _class: JClass,
    use_reranker: jboolean,
    learning_enabled: jboolean,
    cs_ai_boost: jboolean,
    chinese_punct: jboolean,
) {
    if let Ok(mut guard) = engine_slot().lock() {
        if let Some(ref mut eng) = *guard {
            eng.set_settings(
                use_reranker != JNI_FALSE,
                learning_enabled != JNI_FALSE,
                cs_ai_boost != JNI_FALSE,
                chinese_punct != JNI_FALSE,
            );
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeSetMode(
    mut env: JNIEnv,
    _class: JClass,
    is_chinese: jboolean,
    context_before: JString,
) -> jstring {
    let ctx = jstr_to_rust(&mut env, context_before);
    let json = match engine_slot().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(eng) => {
                let res = eng.set_mode(is_chinese != JNI_FALSE, &ctx);
                serde_json::to_string(&res).unwrap_or_else(|_| fallback_action_json("zh"))
            }
            None => fallback_action_json(if is_chinese != JNI_FALSE { "zh" } else { "en" }),
        },
        Err(_) => fallback_action_json("zh"),
    };
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeHandleChar(
    mut env: JNIEnv,
    _class: JClass,
    code_point: jint,
    is_upper: jboolean,
    context_before: JString,
    decode: jboolean,
) -> jstring {
    let ctx = jstr_to_rust(&mut env, context_before);
    let ch = char::from_u32(code_point as u32).unwrap_or('\0');
    if ch == '\0' {
        return to_jstring(&mut env, &fallback_action_json("zh"));
    }
    let json = match engine_slot().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(eng) => {
                let res = eng.handle_char_ext(ch, is_upper != JNI_FALSE, &ctx, decode != JNI_FALSE);
                serde_json::to_string(&res).unwrap_or_else(|_| fallback_action_json("zh"))
            }
            None => fallback_action_json("en"),
        },
        Err(_) => fallback_action_json("zh"),
    };
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeHandleKey(
    mut env: JNIEnv,
    _class: JClass,
    key_kind: jint,
    context_before: JString,
) -> jstring {
    let ctx = jstr_to_rust(&mut env, context_before);
    let json = match engine_slot().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(eng) => {
                let res = eng.handle_special_key(key_kind, &ctx);
                serde_json::to_string(&res).unwrap_or_else(|_| fallback_action_json("zh"))
            }
            None => fallback_action_json("en"),
        },
        Err(_) => fallback_action_json("zh"),
    };
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativePickCandidate(
    mut env: JNIEnv,
    _class: JClass,
    index: jint,
    context_before: JString,
) -> jstring {
    let ctx = jstr_to_rust(&mut env, context_before);
    let json = match engine_slot().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(eng) => {
                let idx = index.max(0) as usize;
                let res = eng.pick_candidate(idx, &ctx);
                serde_json::to_string(&res).unwrap_or_else(|_| fallback_action_json("zh"))
            }
            None => fallback_action_json("en"),
        },
        Err(_) => fallback_action_json("zh"),
    };
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeCommitRaw(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let json = match engine_slot().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(eng) => {
                let res = eng.commit_raw();
                serde_json::to_string(&res).unwrap_or_else(|_| fallback_action_json("zh"))
            }
            None => fallback_action_json("en"),
        },
        Err(_) => fallback_action_json("zh"),
    };
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeCommitWithSuffix(
    mut env: JNIEnv,
    _class: JClass,
    suffix: JString,
    context_before: JString,
) -> jstring {
    let suf = jstr_to_rust(&mut env, suffix);
    let ctx = jstr_to_rust(&mut env, context_before);
    let json = match engine_slot().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(eng) => {
                let res = eng.commit_with_suffix(&suf, &ctx);
                serde_json::to_string(&res).unwrap_or_else(|_| fallback_action_json("zh"))
            }
            None => fallback_action_json("en"),
        },
        Err(_) => fallback_action_json("zh"),
    };
    to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeResetSession(
    _env: JNIEnv,
    _class: JClass,
) {
    if let Ok(mut guard) = engine_slot().lock() {
        if let Some(ref mut eng) = *guard {
            eng.reset_session();
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeSaveUserDict(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    let ok = engine_slot()
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(MobileEngine::save_user_dict))
        .unwrap_or(false);
    if ok {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[no_mangle]
pub extern "system" fn Java_com_pyn_ime_NativeEngine_nativeStats(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let stats = match engine_slot().lock() {
        Ok(guard) => match guard.as_ref() {
            Some(eng) => eng.stats(),
            None => StatsPayload {
                ready: false,
                reranker_loaded: false,
                use_reranker: true,
                learning_enabled: true,
                cs_ai_boost: false,
                total_steps: 0,
                active_vocab_words: 0,
                active_oov_words: 0,
                staged_words: 0,
                active_bigrams: 0,
                last_latency_ms: 0.0,
            },
        },
        Err(_) => StatsPayload {
            ready: false,
            reranker_loaded: false,
            use_reranker: true,
            learning_enabled: true,
            cs_ai_boost: false,
            total_steps: 0,
            active_vocab_words: 0,
            active_oov_words: 0,
            staged_words: 0,
            active_bigrams: 0,
            last_latency_ms: 0.0,
        },
    };
    let json = serde_json::to_string(&stats).unwrap_or_else(|_| r#"{"ready":false}"#.to_owned());
    to_jstring(&mut env, &json)
}
