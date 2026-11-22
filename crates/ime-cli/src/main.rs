//! Command-line front end for the engine.
//!
//! - `ime-cli --jsonl [--model PATH]` serves the evaluation protocol described in
//!   `docs/eval-protocol.md`: one JSON request per line on stdin, one JSON reply per line on stdout.
//! - `ime-cli --interactive [--model PATH]` opens a REPL for interactive trial typing.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ime_core::{ContextSegment, DecoderConfig, Engine, Model};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Segment {
    text: String,
    keys: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    Hello,
    Reset,
    Context { segments: Vec<Segment> },
    Convert { keys: String, n: usize },
    Commit { text: String },
    Undo,
    Bye,
}

fn handle(engine: &mut Engine, request: Request) -> Value {
    match request {
        Request::Hello => json!({
            "ok": true,
            "name": "ime-core",
            "version": env!("CARGO_PKG_VERSION"),
            "capabilities": {"context": true, "learn": engine.learn_config().enabled, "undo": true},
        }),
        Request::Reset => {
            engine.reset();
            json!({"ok": true})
        }
        Request::Context { segments } => {
            engine.set_context(
                segments
                    .into_iter()
                    .map(|seg| ContextSegment {
                        text: seg.text,
                        keys: seg.keys,
                    })
                    .collect(),
            );
            json!({"ok": true, "accepted": true})
        }
        Request::Convert { keys, n } => {
            json!({"ok": true, "candidates": engine.convert(&keys, n)})
        }
        Request::Commit { text } => {
            engine.commit(&text);
            json!({"ok": true, "accepted": true})
        }
        Request::Undo => json!({"ok": true, "accepted": engine.undo()}),
        Request::Bye => json!({"ok": true}),
    }
}

fn serve_jsonl(engine: &mut Engine) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (reply, done) = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let done = matches!(request, Request::Bye);
                (handle(engine, request), done)
            }
            Err(error) => (json!({"ok": false, "error": error.to_string()}), false),
        };
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
        if done {
            break;
        }
    }
    Ok(())
}

fn serve_interactive(engine: &mut Engine) -> io::Result<()> {
    println!("ime-cli 交互试打模式（输入拼音串转换；命令 :pick <序号> | :commit <文本> | :ctx <文本> | :learn | :undo | :reset | :quit）");
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut last_cands: Vec<String> = Vec::new();

    loop {
        let ctx = engine.context_text();
        write!(stdout, "[上文: {ctx}]> ")?;
        stdout.flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd == ":quit" || cmd == ":q" {
            break;
        }
        if cmd == ":reset" {
            engine.reset();
            last_cands.clear();
            println!("已清空上文与临时状态。");
            continue;
        }
        if cmd == ":undo" {
            let ok = engine.undo();
            println!("{}", if ok { "已撤销上一次上屏。" } else { "无可撤销上屏。" });
            continue;
        }
        if cmd == ":learn" {
            let st = engine.user_stats();
            println!(
                "自学习状态: enabled={} | steps={} | 激活词库偏好={} | 激活新词(OOV)={} | 待晋升暂存={} | 上下文二元={}",
                engine.learn_config().enabled,
                st.total_steps,
                st.active_vocab_words,
                st.active_oov_words,
                st.staged_words,
                st.active_bigrams
            );
            for (word, boost, is_oov) in engine.active_user_words().into_iter().take(20) {
                let kind = if is_oov { "OOV" } else { "词库" };
                println!("  - {word} ({kind}, boost=+{boost:.2})");
            }
            continue;
        }
        if let Some(rest) = cmd.strip_prefix(":ctx") {
            let text = rest.trim();
            if text.is_empty() {
                engine.set_context(Vec::new());
            } else {
                engine.set_context(vec![ContextSegment {
                    text: text.to_owned(),
                    keys: None,
                }]);
            }
            last_cands.clear();
            continue;
        }
        if let Some(rest) = cmd.strip_prefix(":commit") {
            let text = rest.trim();
            if text.is_empty() {
                println!("用法: :commit <上屏文本>");
            } else {
                engine.commit(text);
                println!("已上屏: {text}");
                last_cands.clear();
            }
            continue;
        }
        if let Some(rest) = cmd.strip_prefix(":pick") {
            let idx = rest.trim().parse::<usize>().ok().filter(|&n| n >= 1);
            match idx.and_then(|n| last_cands.get(n - 1).cloned()) {
                Some(picked) => {
                    engine.commit(&picked);
                    println!("已上屏: {picked}");
                    last_cands.clear();
                }
                None => println!("无效候选序号，请输入 1..={}。", last_cands.len()),
            }
            continue;
        }

        let t0 = Instant::now();
        last_cands = engine.convert(cmd, 10);
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if last_cands.is_empty() {
            println!("（无候选，耗时 {elapsed_ms:.2} ms）");
        } else {
            for (i, cand) in last_cands.iter().enumerate() {
                print!("{}. {}  ", i + 1, cand);
            }
            println!("({elapsed_ms:.2} ms)");
        }
    }
    Ok(())
}

struct CliArgs {
    jsonl: bool,
    interactive: bool,
    model_path: Option<PathBuf>,
    reranker_path: Option<PathBuf>,
    user_dict_path: Option<PathBuf>,
    no_model: bool,
    no_reranker: bool,
    no_learn: bool,
    score_candidates: bool,
    ctx_arg: String,
    config: DecoderConfig,
}

fn parse_f32_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<f32, String> {
    let val = args
        .next()
        .ok_or_else(|| format!("{flag} requires a float value"))?;
    val.parse::<f32>()
        .map_err(|_| format!("invalid float for {flag}: {val}"))
}

fn parse_usize_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<usize, String> {
    let val = args
        .next()
        .ok_or_else(|| format!("{flag} requires an integer value"))?;
    val.parse::<usize>()
        .map_err(|_| format!("invalid integer for {flag}: {val}"))
}

fn parse_args() -> Result<CliArgs, String> {
    let mut jsonl = false;
    let mut interactive = false;
    let mut model_path: Option<PathBuf> = None;
    let mut reranker_path: Option<PathBuf> = None;
    let mut user_dict_path: Option<PathBuf> = None;
    let mut no_model = false;
    let mut no_reranker = false;
    let mut no_learn = false;
    let mut score_candidates = false;
    let mut ctx_arg = String::new();
    let mut config = DecoderConfig::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--jsonl" => jsonl = true,
            "--interactive" | "-i" => interactive = true,
            "--no-model" => no_model = true,
            "--no-reranker" => no_reranker = true,
            "--no-learn" => no_learn = true,
            "--score-candidates" => score_candidates = true,
            "--ctx" => {
                ctx_arg = args.next().ok_or_else(|| "--ctx requires text".to_owned())?;
            }
            "--model" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--model requires a path".to_owned())?;
                model_path = Some(PathBuf::from(path));
            }
            "--reranker" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--reranker requires a path".to_owned())?;
                reranker_path = Some(PathBuf::from(path));
            }
            "--user-dict" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--user-dict requires a path".to_owned())?;
                user_dict_path = Some(PathBuf::from(path));
            }
            "--eos-weight" => config.eos_weight = parse_f32_arg(&mut args, "--eos-weight")?,
            "--short-eos-weight" => {
                config.short_nocontext_eos_weight = parse_f32_arg(&mut args, "--short-eos-weight")?
            }
            "--reading-weight" => {
                config.reading_weight = parse_f32_arg(&mut args, "--reading-weight")?
            }
            "--word-penalty" => config.word_penalty = parse_f32_arg(&mut args, "--word-penalty")?,
            "--abbrev-penalty" => {
                config.abbrev_penalty = parse_f32_arg(&mut args, "--abbrev-penalty")?
            }
            "--neural-weight" => {
                config.neural_weight = parse_f32_arg(&mut args, "--neural-weight")?
            }
            "--neural-eos-weight" => {
                config.neural_eos_weight = parse_f32_arg(&mut args, "--neural-eos-weight")?
            }
            "--rerank-top-k" => {
                config.rerank_top_k = parse_usize_arg(&mut args, "--rerank-top-k")?
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(CliArgs {
        jsonl,
        interactive,
        model_path,
        reranker_path,
        user_dict_path,
        no_model,
        no_reranker,
        no_learn,
        score_candidates,
        ctx_arg,
        config,
    })
}

fn load_optional_reranker(cli: &CliArgs) -> io::Result<Option<ime_core::Reranker>> {
    if cli.no_model || cli.no_reranker {
        return Ok(None);
    }
    if let Some(ref path) = cli.reranker_path {
        return ime_core::Reranker::load(path).map(Some);
    }
    if let Ok(env_path) = std::env::var("IME_RERANKER_PATH") {
        if !env_path.is_empty() {
            return ime_core::Reranker::load(Path::new(&env_path)).map(Some);
        }
    }
    let default_path = Path::new("models/reranker.bin");
    if default_path.exists() {
        return ime_core::Reranker::load(default_path).map(Some);
    }
    Ok(None)
}

fn resolve_engine(cli: &CliArgs) -> io::Result<Engine> {
    let mut engine = if cli.no_model {
        Engine::new()
    } else if let Some(ref path) = cli.model_path {
        let model = Model::load(path)?;
        Engine::with_model(Arc::new(model))
    } else if let Ok(env_path) = std::env::var("IME_MODEL_PATH") {
        let p = Path::new(&env_path);
        if !env_path.is_empty() {
            let model = Model::load(p)?;
            Engine::with_model(Arc::new(model))
        } else {
            let default_path = Path::new("models/ime_model.bin");
            if default_path.exists() {
                let model = Model::load(default_path)?;
                Engine::with_model(Arc::new(model))
            } else {
                Engine::new()
            }
        }
    } else {
        let default_path = Path::new("models/ime_model.bin");
        if default_path.exists() {
            let model = Model::load(default_path)?;
            Engine::with_model(Arc::new(model))
        } else {
            Engine::new()
        }
    };
    if let Some(reranker) = load_optional_reranker(cli)? {
        engine.set_reranker(Some(Arc::new(reranker)));
    }
    engine.set_config(cli.config);
    if cli.no_learn {
        engine.set_learning_enabled(false);
    }
    if let Some(ref path) = cli.user_dict_path {
        if path.exists() {
            engine.load_user_dict(path)?;
        }
    }
    Ok(engine)
}

fn main() -> io::Result<()> {
    let cli = match parse_args() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!("usage: ime-cli (--jsonl | --interactive | --score-candidates) [--model PATH] [--reranker PATH] [--user-dict PATH] [--no-model] [--no-reranker] [--no-learn]");
            std::process::exit(2);
        }
    };
    if cli.score_candidates {
        let rr = load_optional_reranker(&cli)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "reranker model not found")
        })?;
        let stdin = io::stdin();
        let cands: Vec<String> = stdin
            .lock()
            .lines()
            .map_while(Result::ok)
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        let refs: Vec<&str> = cands.iter().map(String::as_str).collect();
        let scores = rr.score_candidates(&cli.ctx_arg, &refs, cli.config.neural_eos_weight);
        println!("{}", serde_json::json!({ "scores": scores }));
        return Ok(());
    }
    if !cli.jsonl && !cli.interactive {
        eprintln!("usage: ime-cli (--jsonl | --interactive) [--model PATH] [--reranker PATH] [--user-dict PATH] [--no-model] [--no-reranker] [--no-learn]");
        std::process::exit(2);
    }

    let mut engine = match resolve_engine(&cli) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("failed to load model: {err}");
            std::process::exit(1);
        }
    };

    if cli.jsonl {
        serve_jsonl(&mut engine)?;
    } else {
        serve_interactive(&mut engine)?;
    }
    if let Some(ref path) = cli.user_dict_path {
        engine.save_user_dict(path)?;
    }
    Ok(())
}
