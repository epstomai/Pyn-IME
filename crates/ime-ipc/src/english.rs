//! Case-preserving English prefix completion list (`ENGLISH_WORDS`).

/// Common English, dev, CLI, AI, and OS terms sorted strictly in ASCII lexicographical order.
/// Crucially excludes Chinese trailing-abbreviation patterns and common pure Chinese abbrevs.
const ENGLISH_WORDS: &[&str] = &[
    "about", "above", "accept", "access", "account", "action", "active", "actual", "add", "addr",
    "address", "admin", "after", "again", "agent", "agree", "alert", "alias", "align", "all",
    "allow", "almost", "alone", "along", "alpha", "already", "also", "alter", "always", "among",
    "amount", "android", "angle", "angular", "another", "answer", "any", "anyone", "api", "app",
    "apple", "apply", "arch", "area", "arg", "args", "argv", "around", "array", "arrow",
    "ask", "assert", "asset", "assign", "async", "atom", "attach", "audio", "auth", "author",
    "auto", "aux", "avail", "avatar", "await", "award", "away", "awesome", "awk", "aws",
    "axis", "azure", "back", "backup", "bad", "badge", "balance", "bar", "base", "bash",
    "basic", "batch", "beam", "because", "become", "before", "begin", "behind", "below", "bench",
    "best", "beta", "better", "between", "beyond", "big", "bin", "binary", "bind", "bit",
    "bitmap", "bits", "black", "blank", "block", "blog", "blue", "board", "body", "book",
    "bool", "boost", "boot", "border", "both", "bottom", "bound", "box", "branch", "brand",
    "break", "bridge", "bright", "bring", "broad", "broker", "brown", "browse", "browser", "brush",
    "bucket", "buf", "buffer", "bug", "build", "builder", "builtin", "bulk", "bundle", "burn",
    "bus", "button", "buy", "byte", "bytes", "cache", "calc", "call", "callback", "caller",
    "cam", "camera", "can", "cancel", "canvas", "cap", "caps", "card", "care", "cargo",
    "carry", "case", "cast", "cat", "catch", "cause", "cell", "center", "cert", "cfg",
    "chain", "change", "channel", "char", "chart", "chat", "check", "child", "chip", "chmod",
    "choice", "choose", "chrome", "chunk", "ci", "city", "claim", "class", "clean", "clear",
    "cli", "click", "client", "clip", "clock", "clone", "close", "cloud", "cluster", "cmake",
    "cmd", "cmp", "code", "codex", "coffee", "col", "cold", "color", "column", "com",
    "combo", "come", "command", "commit", "common", "compact", "compare", "compile", "complex", "concat",
    "conda", "conf", "config", "confirm", "connect", "console", "const", "contact", "contain", "content",
    "context", "continue", "control", "convert", "cookie", "cool", "copy", "core", "cost", "could",
    "count", "counter", "course", "cover", "cpu", "crack", "crash", "crate", "create", "cred",
    "cron", "crop", "cross", "crt", "crypto", "css", "csv", "ctrl", "cuda", "curl",
    "current", "cursor", "curve", "custom", "cut", "cycle", "daemon", "dark", "dash", "data",
    "date", "day", "days", "dead", "deal", "debug", "decay", "decode", "decoder", "deep",
    "default", "defer", "define", "deg", "del", "delay", "delete", "delta", "demo", "deny",
    "dep", "deploy", "depth", "derive", "desc", "design", "desk", "desktop", "detail", "detect",
    "dev", "device", "dialog", "diff", "digit", "dim", "dir", "direct", "dirty", "disable",
    "disk", "display", "dist", "div", "dll", "dns", "do", "doc", "docker", "docs",
    "dom", "domain", "done", "dot", "double", "down", "draft", "drag", "draw", "drive",
    "driver", "drop", "dst", "dual", "due", "dump", "dup", "during", "dvd", "dyn",
    "each", "early", "earth", "ease", "east", "easy", "echo", "edge", "edit", "editor",
    "effect", "either", "elem", "else", "email", "embed", "empty", "enable", "encode", "encoder",
    "end", "engine", "enough", "ensure", "enter", "entry", "enum", "env", "eof", "epoch",
    "equal", "err", "errno", "error", "esc", "escape", "eval", "even", "event", "every",
    "exact", "exam", "excel", "except", "exec", "exist", "exit", "expand", "expect", "expert",
    "expire", "export", "expr", "ext", "extend", "extra", "eye", "face", "fact", "factor",
    "fail", "false", "family", "far", "fast", "fatal", "favor", "fax", "feature", "feed",
    "feel", "fetch", "few", "fft", "field", "fig", "file", "files", "fill", "filter",
    "final", "find", "fine", "finish", "fire", "first", "fit", "fix", "fixed", "flag",
    "flags", "flash", "flat", "flex", "flip", "float", "floor", "flow", "flush", "fly",
    "fmt", "focus", "fold", "folder", "follow", "font", "foo", "food", "foot", "for",
    "force", "fork", "form", "format", "forward", "found", "frame", "free", "fresh", "friend",
    "from", "front", "ftp", "full", "fun", "func", "function", "future", "gain", "game",
    "gap", "gas", "gate", "gcc", "gdb", "gem", "gemini", "gemm", "gen", "general",
    "get", "git", "github", "gitlab", "give", "glad", "glass", "glibc", "global", "glow",
    "gnu", "go", "goal", "gold", "good", "google", "gpu", "grab", "grace", "grade",
    "graph", "gray", "great", "green", "grep", "grid", "ground", "group", "grow", "guard",
    "guess", "guest", "gui", "guid", "guide", "gzip", "hack", "half", "halt", "hand",
    "handle", "hang", "happy", "hard", "hash", "have", "head", "header", "health", "heap",
    "heart", "heat", "heavy", "height", "hello", "help", "here", "hex", "hidden", "hide",
    "high", "hint", "hist", "history", "hit", "hold", "home", "hook", "hop", "hope",
    "host", "hot", "hotkey", "hour", "house", "hover", "how", "html", "http", "https",
    "hub", "huge", "human", "icon", "id", "idea", "ideal", "ident", "idle", "if",
    "ignore", "image", "img", "impl", "import", "in", "inbox", "inc", "inch", "include",
    "index", "info", "init", "inline", "inner", "input", "insert", "inside", "install", "int",
    "intent", "inter", "into", "intro", "io", "ip", "ipc", "is", "iso", "issue",
    "it", "item", "items", "iter", "its", "java", "job", "jobs", "join", "json",
    "jsonl", "jsx", "jump", "just", "jvm", "keep", "kernel", "key", "keys", "kill",
    "kind", "kit", "know", "known", "lab", "label", "lang", "large", "last", "late",
    "later", "launch", "layer", "layout", "lazy", "lead", "leaf", "leak", "learn", "least",
    "leave", "left", "len", "length", "less", "let", "letter", "level", "lib", "life",
    "lift", "light", "like", "limit", "line", "link", "linux", "list", "listen", "lite",
    "live", "load", "local", "lock", "log", "logic", "login", "logo", "logs", "long",
    "look", "lookup", "loop", "loss", "lost", "lot", "loud", "love", "low", "lower",
    "lru", "lua", "mac", "macos", "macro", "magic", "mail", "main", "major", "make",
    "man", "manage", "manual", "many", "map", "margin", "mark", "mask", "master", "match",
    "math", "matrix", "max", "maybe", "mcp", "mean", "media", "medium", "meet", "mem",
    "member", "memory", "menu", "merge", "mesh", "meta", "metal", "method", "metric", "mid",
    "middle", "might", "min", "mind", "mini", "minus", "minute", "mirror", "miss", "mix",
    "mkdir", "mod", "mode", "model", "models", "modern", "modify", "module", "mon", "money",
    "monitor", "month", "more", "most", "mount", "mouse", "move", "msg", "msvc", "much",
    "multi", "music", "must", "mut", "mutex", "name", "nano", "native", "nav", "near",
    "need", "neg", "nest", "net", "network", "never", "new", "news", "next", "nice",
    "night", "nil", "no", "node", "nodes", "noise", "none", "norm", "normal", "north",
    "not", "note", "notes", "notice", "now", "npu", "null", "num", "number", "numpy",
    "nvidia", "obj", "object", "odd", "of", "off", "offer", "office", "often", "ok",
    "okay", "old", "on", "once", "one", "only", "open", "openai", "ops", "opt",
    "option", "or", "order", "org", "origin", "os", "other", "our", "out", "outer",
    "output", "over", "own", "owner", "pack", "package", "pad", "page", "paint", "pair",
    "pal", "panel", "panic", "paper", "param", "parent", "parse", "parser", "part", "pass",
    "paste", "patch", "path", "pause", "pay", "pdf", "peak", "peer", "pen", "pending",
    "per", "percent", "perf", "period", "perm", "phase", "phone", "photo", "php", "pic",
    "pick", "pid", "pill", "pin", "ping", "pip", "pipe", "pixel", "place", "plain",
    "plan", "play", "please", "plot", "plugin", "plus", "png", "point", "poll", "pool",
    "pop", "port", "pos", "post", "pow", "power", "pre", "prefix", "press", "pretty",
    "prev", "preview", "price", "prime", "print", "prior", "priv", "pro", "probe", "proc",
    "process", "prod", "profile", "prog", "program", "proj", "project", "prompt", "proof", "prop",
    "props", "proto", "proxy", "prune", "ptr", "pub", "public", "pull", "pump", "pure",
    "push", "put", "pwd", "python", "qemu", "query", "queue", "quick", "quit", "quiz",
    "quote", "race", "rad", "radio", "rand", "random", "range", "rank", "rare", "rate",
    "ratio", "raw", "ray", "reach", "react", "read", "readme", "ready", "real", "reason",
    "reboot", "rec", "record", "rect", "red", "redis", "redo", "reduce", "ref", "refresh",
    "reg", "regex", "region", "reject", "rel", "release", "reload", "remain", "remote", "remove",
    "rename", "render", "rep", "repeat", "reply", "repo", "report", "req", "request", "require",
    "rerank", "reset", "resize", "resolve", "resp", "rest", "restart", "result", "ret", "retry",
    "return", "rev", "review", "rich", "right", "ring", "role", "roll", "room", "root",
    "rope", "round", "route", "router", "row", "rows", "rpc", "rss", "rule", "rules",
    "run", "runner", "rust", "rustc", "safe", "salt", "same", "sample", "sandbox", "save",
    "say", "scale", "scan", "scene", "schema", "scheme", "scope", "score", "screen", "script",
    "scroll", "sdk", "search", "sec", "second", "secret", "section", "secure", "seed", "seek",
    "seem", "select", "self", "send", "sense", "sent", "seq", "serial", "serve", "server",
    "service", "session", "set", "sets", "setup", "seven", "sha", "shadow", "shape", "share",
    "sharp", "she", "sheet", "shell", "shift", "ship", "short", "should", "show", "shrink",
    "side", "sign", "signal", "silu", "simd", "simple", "since", "single", "sink", "site",
    "size", "skip", "sleep", "slice", "slide", "slot", "slow", "small", "smart", "smoke",
    "smooth", "snap", "so", "socket", "soft", "solid", "solve", "some", "soon", "sort",
    "sound", "source", "south", "space", "span", "sparse", "spawn", "spec", "speed", "spell",
    "spin", "split", "spring", "sql", "sqlite", "src", "ssh", "ssl", "stable", "stack",
    "stage", "stand", "start", "stat", "state", "static", "stats", "status", "stay", "std",
    "stdout", "step", "steps", "stick", "still", "stop", "store", "story", "str", "stream",
    "stride", "string", "strip", "struct", "stub", "studio", "style", "sub", "submit", "sudo",
    "sum", "super", "support", "sure", "svg", "swap", "sweep", "sweet", "swift", "swing",
    "switch", "sym", "symbol", "sync", "syntax", "sys", "system", "tab", "table", "tabs",
    "tag", "tags", "tail", "take", "talk", "tar", "target", "task", "tasks", "tcp",
    "team", "tech", "tell", "temp", "template", "term", "test", "tests", "text", "than",
    "thank", "thanks", "that", "the", "their", "them", "theme", "then", "there", "these",
    "they", "thin", "thing", "think", "this", "those", "thread", "three", "throw", "thumb",
    "tick", "tidy", "tie", "tile", "till", "time", "timer", "tiny", "tip", "title",
    "tmp", "to", "todo", "toggle", "token", "toml", "tone", "too", "tool", "tools",
    "top", "topic", "total", "touch", "trace", "track", "train", "trait", "trans", "tree",
    "trend", "trie", "trigger", "trim", "true", "trunk", "trust", "try", "tsf", "tsx",
    "tty", "tune", "tuple", "turn", "twice", "two", "txt", "type", "types", "ubuntu",
    "udp", "ui", "uid", "uint", "ultra", "umask", "undef", "under", "undo", "unfold",
    "uni", "union", "unique", "unit", "unix", "unk", "unless", "unlink", "unlock", "unpack",
    "unset", "until", "up", "update", "upload", "upon", "upper", "uri", "url", "us",
    "usage", "usb", "use", "used", "user", "users", "using", "utf", "util", "utils",
    "uuid", "val", "valid", "value", "values", "var", "vars", "vec", "vector", "vendor",
    "ver", "verify", "version", "versus", "very", "via", "video", "view", "views", "vim",
    "virt", "virtual", "visual", "vite", "void", "vol", "volume", "vscode", "vue", "wait",
    "wake", "walk", "wall", "want", "warm", "warn", "warning", "was", "watch", "water",
    "wave", "way", "ways", "we", "weak", "web", "webhook", "week", "weight", "welcome",
    "well", "were", "west", "what", "wheel", "when", "where", "which", "while", "white",
    "who", "whole", "whom", "whose", "why", "wide", "widget", "width", "wifi", "win",
    "win32", "window", "windows", "wipe", "wire", "with", "within", "without", "word", "words",
    "work", "worker", "world", "worth", "would", "wrap", "write", "writer", "wrong", "www",
    "x64", "x86", "xml", "xor", "yaml", "yarn", "year", "yes", "yet", "yield",
    "yml", "you", "young", "your", "zero", "zig", "zip", "zone", "zoom", "zsh",
];

/// Returns the exact typed `raw_keys` as candidate #1, optionally followed by case-matched
/// prefix completions from `ENGLISH_WORDS` (up to `max_results`).
pub fn suggest_english_candidates(raw_keys: &str, max_results: usize) -> Vec<String> {
    if raw_keys.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(max_results.min(16));
    out.push(raw_keys.to_owned());
    if out.len() >= max_results
        || raw_keys.len() < 2
        || !raw_keys.bytes().all(|b| b.is_ascii_alphabetic())
    {
        return out;
    }
    let bytes = raw_keys.as_bytes();
    let has_internal_upper = bytes[1..].iter().any(|b| b.is_ascii_uppercase());
    let has_lower = bytes.iter().any(|b| b.is_ascii_lowercase());
    if has_internal_upper && has_lower {
        return out;
    }

    let lower = raw_keys.to_ascii_lowercase();
    let start = ENGLISH_WORDS.partition_point(|&w| w < lower.as_str());
    for &word in &ENGLISH_WORDS[start..] {
        if !word.starts_with(lower.as_str()) {
            break;
        }
        let styled = apply_case_style(word, raw_keys);
        if styled != raw_keys {
            out.push(styled);
            if out.len() >= max_results {
                break;
            }
        }
    }
    out
}

fn apply_case_style(word: &str, raw_keys: &str) -> String {
    let bytes = raw_keys.as_bytes();
    let all_upper = bytes.len() >= 2 && bytes.iter().all(|b| b.is_ascii_uppercase());
    if all_upper {
        return word.to_ascii_uppercase();
    }
    let title_case = bytes.first().is_some_and(|b| b.is_ascii_uppercase())
        && bytes[1..].iter().all(|b| b.is_ascii_lowercase());
    if title_case {
        let mut s = String::with_capacity(word.len());
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            s.push(first.to_ascii_uppercase());
            s.push_str(chars.as_str());
        }
        return s;
    }
    word.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_words_table_is_strictly_sorted_and_unique() {
        for pair in ENGLISH_WORDS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "ENGLISH_WORDS must be strictly sorted & unique: {:?} vs {:?}",
                pair[0],
                pair[1]
            );
            assert!(pair[0].bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        }
    }

    #[test]
    fn suggest_english_candidates_preserves_exact_input_and_case() {
        let hel = suggest_english_candidates("hel", 5);
        assert_eq!(hel.first().map(String::as_str), Some("hel"));
        assert!(hel.contains(&"hello".to_owned()));
        assert!(hel.contains(&"help".to_owned()));

        let title = suggest_english_candidates("Hel", 5);
        assert_eq!(title.first().map(String::as_str), Some("Hel"));
        assert!(title.contains(&"Hello".to_owned()));
        assert!(title.contains(&"Help".to_owned()));

        let upper = suggest_english_candidates("HT", 5);
        assert_eq!(upper.first().map(String::as_str), Some("HT"));
        assert!(upper.iter().any(|w| w.starts_with("HT") && w.len() > 2));

        assert_eq!(suggest_english_candidates("NiloToon", 5), vec!["NiloToon"]);
        assert_eq!(suggest_english_candidates("InP", 5), vec!["InP"]);
        assert_eq!(suggest_english_candidates("a", 5), vec!["a"]);
        assert!(suggest_english_candidates("", 5).is_empty());
    }
}
