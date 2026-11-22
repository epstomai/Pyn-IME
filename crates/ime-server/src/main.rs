//! Single local inference and guarded-learning service over Win32 Named Pipes.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ime_core::decoder::decode_with_user;
use ime_core::user_dict::LearnDelta;
use ime_core::{DecoderConfig, LearnConfig, Model, Reranker, UserMemory};
use ime_ipc::pipe::{to_wide_null, NamedPipeClient, NamedPipeServerInstance};
use ime_ipc::protocol::{read_frame, write_frame, IpcRequest, IpcResponse, DEFAULT_PIPE_NAME};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows_core::PCWSTR;

const MUTEX_NAME: &str = r"Local\InputEngineServerMutex_v1";
const SAVE_DEBOUNCE_SECS: u64 = 10;
const MAX_SESSIONS: usize = 64;

struct CliArgs {
    pipe_name: String,
    model_path: Option<PathBuf>,
    reranker_path: Option<PathBuf>,
    no_reranker: bool,
    user_dict_path: Option<PathBuf>,
    no_learn: bool,
    ping: bool,
    shutdown: bool,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut pipe_name = DEFAULT_PIPE_NAME.to_owned();
    let mut model_path: Option<PathBuf> = None;
    let mut reranker_path: Option<PathBuf> = None;
    let mut no_reranker = false;
    let mut user_dict_path: Option<PathBuf> = None;
    let mut no_learn = false;
    let mut ping = false;
    let mut shutdown = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pipe" => {
                pipe_name = args
                    .next()
                    .ok_or_else(|| "--pipe requires a pipe name".to_owned())?;
            }
            "--model" => {
                let p = args
                    .next()
                    .ok_or_else(|| "--model requires a path".to_owned())?;
                model_path = Some(PathBuf::from(p));
            }
            "--reranker" => {
                let p = args
                    .next()
                    .ok_or_else(|| "--reranker requires a path".to_owned())?;
                reranker_path = Some(PathBuf::from(p));
            }
            "--no-reranker" => no_reranker = true,
            "--user-dict" => {
                let p = args
                    .next()
                    .ok_or_else(|| "--user-dict requires a path".to_owned())?;
                user_dict_path = Some(PathBuf::from(p));
            }
            "--no-learn" => no_learn = true,
            "--ping" => ping = true,
            "--shutdown" => shutdown = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(CliArgs {
        pipe_name,
        model_path,
        reranker_path,
        no_reranker,
        user_dict_path,
        no_learn,
        ping,
        shutdown,
    })
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

fn resolve_model_path(explicit: Option<PathBuf>, env_var: &str, filename: &str) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p);
    }
    if let Ok(env_val) = std::env::var(env_var) {
        if !env_val.is_empty() {
            return Some(PathBuf::from(env_val));
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = exe_dir() {
        candidates.push(dir.join("models").join(filename));
        candidates.push(dir.join(filename));
    }
    candidates.push(PathBuf::from("models").join(filename));
    candidates.into_iter().find(|p| p.exists())
}

fn resolve_user_dict_path(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        if !local_app_data.is_empty() {
            return PathBuf::from(local_app_data)
                .join("InputEngine")
                .join("user_dict.tsv");
        }
    }
    PathBuf::from("user_dict.tsv")
}

struct MutexGuard {
    handle: HANDLE,
}

impl Drop for MutexGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn acquire_single_instance_mutex() -> io::Result<Option<MutexGuard>> {
    let wide = to_wide_null(MUTEX_NAME);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0))?;
    let err = unsafe { GetLastError() };
    if err == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(MutexGuard { handle }))
}

#[derive(Default)]
struct ClientSessionState {
    last_keys: String,
    last_base_top1: Option<String>,
    last_current_top1: Option<String>,
    last_delta: Option<LearnDelta>,
    last_active_seq: u64,
}

struct ServerState {
    user: UserMemory,
    sessions: HashMap<u64, ClientSessionState>,
    dirty: bool,
    last_save: Instant,
    seq: u64,
    started_at: Instant,
}

impl ServerState {
    fn touch_session(&mut self, session_id: u64) -> &mut ClientSessionState {
        self.seq = self.seq.saturating_add(1);
        let seq = self.seq;
        if !self.sessions.contains_key(&session_id) && self.sessions.len() >= MAX_SESSIONS {
            if let Some(oldest_id) = self
                .sessions
                .iter()
                .min_by_key(|(_, st)| st.last_active_seq)
                .map(|(&id, _)| id)
            {
                self.sessions.remove(&oldest_id);
            }
        }
        let sess = self.sessions.entry(session_id).or_default();
        sess.last_active_seq = seq;
        sess
    }
}

struct ServerCtx {
    model: Arc<Model>,
    reranker: Option<Arc<Reranker>>,
    decoder_cfg: DecoderConfig,
    user_dict_path: PathBuf,
    pipe_name: String,
    shutdown: AtomicBool,
    state: Mutex<ServerState>,
}

fn save_user_dict_atomic(user: &UserMemory, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp_path = path.with_extension("tsv.tmp");
    user.save(&tmp_path)?;
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

fn maybe_flush_user_dict(ctx: &ServerCtx, force: bool) {
    let snapshot: Option<UserMemory> = {
        let mut st = match ctx.state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !st.dirty {
            return;
        }
        if !force && st.last_save.elapsed() < Duration::from_secs(SAVE_DEBOUNCE_SECS) {
            return;
        }
        st.dirty = false;
        st.last_save = Instant::now();
        Some(st.user.clone())
    };
    if let Some(user) = snapshot {
        if let Err(err) = save_user_dict_atomic(&user, &ctx.user_dict_path) {
            eprintln!("ime-server: failed to save user dict: {err}");
        }
    }
}

fn handle_request(ctx: &ServerCtx, req: IpcRequest) -> IpcResponse {
    match req {
        IpcRequest::Ping => {
            let learn_enabled = ctx
                .state
                .lock()
                .map(|s| s.user.config().enabled)
                .unwrap_or(false);
            IpcResponse::Pong {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                model_loaded: true,
                reranker_loaded: ctx.reranker.is_some(),
                learning_enabled: learn_enabled,
            }
        }
        IpcRequest::Convert {
            session_id,
            context,
            keys,
            limit,
        } => {
            let started = Instant::now();
            let mut st = match ctx.state.lock() {
                Ok(g) => g,
                Err(_) => {
                    return IpcResponse::Error {
                        message: "server state mutex poisoned".into(),
                    }
                }
            };
            let outcome = decode_with_user(
                &ctx.model,
                ctx.reranker.as_deref(),
                Some(&st.user),
                &keys,
                &context,
                limit,
                &ctx.decoder_cfg,
            );
            let sess = st.touch_session(session_id);
            sess.last_keys = keys;
            sess.last_base_top1 = outcome.base_top1;
            sess.last_current_top1 = outcome.candidates.first().cloned();
            let latency_ms = started.elapsed().as_secs_f32() * 1000.0;
            IpcResponse::Candidates {
                candidates: outcome.candidates,
                latency_ms,
            }
        }
        IpcRequest::Commit {
            session_id,
            context,
            keys,
            text,
        } => {
            let mut st = match ctx.state.lock() {
                Ok(g) => g,
                Err(_) => {
                    return IpcResponse::Error {
                        message: "server state mutex poisoned".into(),
                    }
                }
            };
            let (base_top1, cur_top1) = {
                let sess = st.touch_session(session_id);
                if sess.last_keys == keys && !keys.is_empty() {
                    (sess.last_base_top1.take(), sess.last_current_top1.take())
                } else {
                    (None, None)
                }
            };
            let (base_top1, cur_top1) = if base_top1.is_none() && !keys.is_empty() {
                let outcome = decode_with_user(
                    &ctx.model,
                    ctx.reranker.as_deref(),
                    Some(&st.user),
                    &keys,
                    &context,
                    10,
                    &ctx.decoder_cfg,
                );
                (outcome.base_top1, outcome.candidates.first().cloned())
            } else {
                (base_top1, cur_top1)
            };

            let delta = st.user.record_commit(
                &ctx.model,
                &keys,
                &text,
                &context,
                base_top1.as_deref(),
                cur_top1.as_deref(),
            );
            {
                let sess = st.touch_session(session_id);
                sess.last_keys.clear();
                sess.last_delta = Some(delta);
            }
            st.dirty = true;
            drop(st);
            maybe_flush_user_dict(ctx, false);
            IpcResponse::Ack { ok: true }
        }
        IpcRequest::Undo { session_id } => {
            let mut st = match ctx.state.lock() {
                Ok(g) => g,
                Err(_) => {
                    return IpcResponse::Error {
                        message: "server state mutex poisoned".into(),
                    }
                }
            };
            let delta = {
                let sess = st.touch_session(session_id);
                sess.last_delta.take()
            };
            let ok = if let Some(d) = delta {
                st.user.undo_commit(d);
                st.dirty = true;
                true
            } else {
                false
            };
            drop(st);
            if ok {
                maybe_flush_user_dict(ctx, false);
            }
            IpcResponse::Ack { ok }
        }
        IpcRequest::ResetSession { session_id } => {
            if let Ok(mut st) = ctx.state.lock() {
                st.user.flush_compose_run(&ctx.model);
                let sess = st.touch_session(session_id);
                sess.last_keys.clear();
                sess.last_base_top1 = None;
                sess.last_current_top1 = None;
                sess.last_delta = None;
            }
            IpcResponse::Ack { ok: true }
        }
        IpcRequest::Stats => {
            let st = match ctx.state.lock() {
                Ok(g) => g,
                Err(_) => {
                    return IpcResponse::Error {
                        message: "server state mutex poisoned".into(),
                    }
                }
            };
            let u = st.user.stats();
            IpcResponse::Stats {
                total_steps: u.total_steps,
                active_vocab_words: u.active_vocab_words,
                active_oov_words: u.active_oov_words,
                staged_words: u.staged_words,
                active_bigrams: u.active_bigrams,
                uptime_secs: st.started_at.elapsed().as_secs(),
            }
        }
        IpcRequest::Shutdown => {
            ctx.shutdown.store(true, Ordering::SeqCst);
            maybe_flush_user_dict(ctx, true);
            IpcResponse::Ack { ok: true }
        }
    }
}

fn serve_connection(mut pipe: NamedPipeServerInstance, ctx: Arc<ServerCtx>) {
    loop {
        if ctx.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let req: IpcRequest = match read_frame(&mut pipe) {
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                ) =>
            {
                break;
            }
            Err(e) => {
                let _ = write_frame(
                    &mut pipe,
                    &IpcResponse::Error {
                        message: e.to_string(),
                    },
                );
                break;
            }
        };

        let is_shutdown = matches!(req, IpcRequest::Shutdown);
        let resp = handle_request(&ctx, req);
        if write_frame(&mut pipe, &resp).is_err() {
            break;
        }
        if is_shutdown {
            // Wake main accept() loop cleanly if it is waiting on ConnectNamedPipe.
            let _ = NamedPipeClient::connect(&ctx.pipe_name, 200);
            break;
        }
    }
}

fn run_control_command(pipe_name: &str, req: IpcRequest) -> ! {
    match NamedPipeClient::connect(pipe_name, 1000).and_then(|mut c| c.request(&req)) {
        Ok(resp) => {
            println!("{}", serde_json::to_string(&resp).unwrap_or_default());
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("ime-server control error: {err}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = match parse_args() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(2);
        }
    };

    if cli.ping {
        run_control_command(&cli.pipe_name, IpcRequest::Ping);
    }
    if cli.shutdown {
        run_control_command(&cli.pipe_name, IpcRequest::Shutdown);
    }

    let _mutex_guard = if cli.pipe_name == DEFAULT_PIPE_NAME {
        match acquire_single_instance_mutex() {
            Ok(Some(g)) => Some(g),
            Ok(None) => return,
            Err(err) => {
                eprintln!("ime-server mutex error: {err}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let Some(model_path) = resolve_model_path(cli.model_path, "IME_MODEL_PATH", "ime_model.bin")
    else {
        eprintln!("ime-server: cannot locate ime_model.bin");
        std::process::exit(1);
    };
    let model = match Model::load(&model_path) {
        Ok(m) => Arc::new(m),
        Err(err) => {
            eprintln!("ime-server: failed to load model {}: {err}", model_path.display());
            std::process::exit(1);
        }
    };

    let reranker = if cli.no_reranker {
        None
    } else if let Some(rr_path) =
        resolve_model_path(cli.reranker_path, "IME_RERANKER_PATH", "reranker.bin")
    {
        match Reranker::load(&rr_path) {
            Ok(rr) => Some(Arc::new(rr)),
            Err(err) => {
                eprintln!("ime-server: failed to load reranker {}: {err}", rr_path.display());
                None
            }
        }
    } else {
        None
    };

    let user_dict_path = resolve_user_dict_path(cli.user_dict_path);
    let mut learn_cfg = LearnConfig::default();
    if cli.no_learn {
        learn_cfg.enabled = false;
    }
    let user = if user_dict_path.exists() {
        match UserMemory::load(&user_dict_path, &model, learn_cfg) {
            Ok(u) => u,
            Err(err) => {
                eprintln!(
                    "ime-server: warning loading user dict {}: {err}",
                    user_dict_path.display()
                );
                UserMemory::new(learn_cfg)
            }
        }
    } else {
        UserMemory::new(learn_cfg)
    };

    let ctx = Arc::new(ServerCtx {
        model,
        reranker,
        decoder_cfg: DecoderConfig::default(),
        user_dict_path,
        pipe_name: cli.pipe_name.clone(),
        shutdown: AtomicBool::new(false),
        state: Mutex::new(ServerState {
            user,
            sessions: HashMap::new(),
            dirty: false,
            last_save: Instant::now(),
            seq: 0,
            started_at: Instant::now(),
        }),
    });

    while !ctx.shutdown.load(Ordering::SeqCst) {
        let pipe = match NamedPipeServerInstance::create(&ctx.pipe_name) {
            Ok(p) => p,
            Err(err) => {
                eprintln!("ime-server: CreateNamedPipeW failed: {err}");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        if let Err(err) = pipe.accept() {
            eprintln!("ime-server: ConnectNamedPipe failed: {err}");
            continue;
        }
        if ctx.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let ctx_cloned = Arc::clone(&ctx);
        std::thread::spawn(move || serve_connection(pipe, ctx_cloned));
    }

    maybe_flush_user_dict(&ctx, true);
}
