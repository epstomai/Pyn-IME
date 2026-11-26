//! Case-preserving English prefix completion list (`ENGLISH_WORDS`) ranked by usage frequency.

use std::cmp::Reverse;

/// Common English, dev, CLI, AI, and OS terms sorted strictly in ASCII lexicographical order,
/// paired with coarse usage-frequency tiers (`50..=250`, higher means more frequent).
/// Crucially excludes Chinese trailing-abbreviation patterns and common pure Chinese abbrevs.
const ENGLISH_WORDS: &[(&str, u8)] = &[
    ("about", 240), ("above", 200), ("accept", 200), ("access", 200), ("account", 200), ("action", 200),
    ("active", 200), ("actual", 200), ("add", 200), ("addr", 200), ("address", 200), ("admin", 200),
    ("after", 240), ("again", 200), ("agent", 200), ("agentic", 160), ("agree", 200), ("alert", 200),
    ("alias", 200), ("align", 200), ("alipay", 160), ("all", 240), ("allow", 200), ("almost", 200),
    ("alone", 200), ("along", 200), ("alpha", 200), ("already", 200), ("also", 240), ("alter", 160),
    ("always", 200), ("among", 200), ("amount", 200), ("android", 200), ("angle", 160), ("angular", 120),
    ("another", 200), ("answer", 200), ("any", 240), ("anyone", 200), ("api", 240), ("app", 240),
    ("apple", 200), ("apply", 200), ("arch", 200), ("area", 200), ("arg", 200), ("args", 200),
    ("argv", 80), ("around", 200), ("array", 240), ("arrow", 200), ("arxiv", 160), ("ask", 240),
    ("assert", 200), ("asset", 200), ("assign", 200), ("async", 240), ("atom", 160), ("attach", 200),
    ("attention", 200), ("audio", 200), ("auth", 200), ("author", 200), ("auto", 240), ("aux", 80),
    ("avail", 200), ("avatar", 200), ("avx", 160), ("await", 240), ("award", 160), ("away", 200),
    ("awesome", 120), ("awk", 80), ("aws", 80), ("axis", 160), ("axum", 120), ("azure", 200),
    ("back", 240), ("backup", 200), ("bad", 200), ("badge", 200), ("balance", 200), ("bar", 200),
    ("base", 240), ("bash", 200), ("basic", 200), ("batch", 200), ("beam", 200), ("because", 200),
    ("become", 200), ("before", 200), ("begin", 200), ("behind", 200), ("below", 200), ("bench", 200),
    ("best", 240), ("beta", 160), ("better", 200), ("between", 200), ("beyond", 200), ("bf16", 160),
    ("big", 200), ("bilibili", 160), ("bin", 240), ("binary", 200), ("bind", 200), ("bit", 200),
    ("bitmap", 80), ("bits", 200), ("black", 200), ("blank", 200), ("block", 200), ("blog", 200),
    ("blue", 200), ("board", 200), ("body", 200), ("book", 200), ("bool", 240), ("boost", 200),
    ("boot", 200), ("border", 200), ("both", 200), ("bottom", 200), ("bound", 200), ("box", 200),
    ("branch", 200), ("brand", 200), ("break", 200), ("bridge", 200), ("bright", 120), ("bring", 200),
    ("broad", 80), ("broker", 80), ("brown", 80), ("browse", 120), ("browser", 200), ("brush", 80),
    ("bucket", 200), ("buf", 200), ("buffer", 200), ("bug", 200), ("build", 240), ("builder", 200),
    ("builtin", 200), ("bulk", 80), ("bun", 120), ("bundle", 200), ("burn", 80), ("bus", 200),
    ("button", 200), ("buy", 200), ("byte", 200), ("bytes", 200), ("cache", 200), ("calc", 200),
    ("call", 240), ("callback", 200), ("caller", 200), ("cam", 80), ("camera", 200), ("can", 240),
    ("cancel", 200), ("canvas", 200), ("cap", 200), ("caps", 80), ("card", 200), ("care", 200),
    ("cargo", 200), ("carry", 200), ("case", 200), ("cast", 200), ("cat", 200), ("catch", 200),
    ("cause", 200), ("cell", 200), ("center", 200), ("cert", 200), ("cfg", 200), ("chain", 200),
    ("change", 200), ("channel", 200), ("char", 240), ("chart", 200), ("chat", 200), ("check", 240),
    ("child", 200), ("chip", 200), ("chmod", 80), ("choice", 200), ("choose", 200), ("chrome", 200),
    ("chunk", 200), ("ci", 200), ("city", 200), ("claim", 200), ("class", 240), ("claude", 200),
    ("clean", 200), ("clear", 240), ("cli", 240), ("click", 200), ("client", 240), ("clip", 200),
    ("clock", 200), ("clone", 200), ("close", 240), ("cloud", 200), ("cloudflare", 160), ("cluster", 200),
    ("cmake", 200), ("cmd", 200), ("cmp", 200), ("code", 240), ("codex", 200), ("coffee", 120),
    ("col", 200), ("cold", 200), ("color", 200), ("column", 200), ("com", 200), ("combo", 160),
    ("come", 160), ("command", 200), ("commit", 240), ("common", 200), ("compact", 200), ("compare", 200),
    ("compile", 200), ("complex", 200), ("concat", 200), ("conda", 200), ("conf", 200), ("config", 240),
    ("confirm", 200), ("connect", 200), ("console", 200), ("const", 240), ("contact", 200), ("contain", 200),
    ("content", 200), ("context", 200), ("continue", 200), ("control", 200), ("convert", 200), ("cookie", 200),
    ("cool", 200), ("copilot", 200), ("copy", 240), ("core", 240), ("cost", 200), ("could", 200),
    ("count", 200), ("counter", 200), ("course", 200), ("cover", 200), ("cpu", 240), ("crack", 80),
    ("crash", 200), ("crate", 200), ("create", 240), ("cred", 160), ("cron", 200), ("crop", 160),
    ("cross", 200), ("crt", 80), ("crypto", 200), ("css", 200), ("csv", 200), ("ctrl", 200),
    ("cuda", 200), ("curl", 200), ("current", 200), ("cursor", 200), ("curve", 200), ("custom", 200),
    ("cut", 200), ("cycle", 200), ("daemon", 200), ("dark", 200), ("dash", 200), ("data", 240),
    ("date", 240), ("day", 240), ("days", 160), ("dead", 160), ("deal", 200), ("debug", 240),
    ("decay", 200), ("decode", 200), ("decoder", 200), ("deep", 200), ("deepseek", 200), ("default", 240),
    ("defer", 200), ("define", 200), ("deg", 80), ("del", 200), ("delay", 200), ("delete", 240),
    ("delta", 200), ("demo", 200), ("deno", 120), ("deny", 200), ("dep", 200), ("deploy", 200),
    ("depth", 200), ("derive", 200), ("desc", 200), ("design", 200), ("desk", 200), ("desktop", 200),
    ("detail", 200), ("detect", 200), ("dev", 240), ("device", 200), ("dialog", 200), ("diff", 200),
    ("digit", 200), ("dim", 200), ("dir", 240), ("direct", 200), ("dirty", 200), ("disable", 200),
    ("discord", 160), ("disk", 200), ("display", 200), ("dist", 200), ("div", 200), ("dll", 200),
    ("dns", 200), ("do", 240), ("doc", 240), ("docker", 200), ("dockerfile", 160), ("docs", 240),
    ("dom", 200), ("domain", 200), ("done", 240), ("dot", 200), ("double", 200), ("down", 240),
    ("draft", 200), ("drag", 200), ("draw", 200), ("drive", 200), ("driver", 200), ("drop", 200),
    ("dst", 80), ("dual", 200), ("due", 200), ("dump", 200), ("dup", 160), ("during", 200),
    ("dvd", 80), ("dyn", 200), ("each", 200), ("early", 200), ("earth", 200), ("ease", 200),
    ("east", 80), ("easy", 200), ("echo", 200), ("edge", 200), ("edit", 240), ("editor", 200),
    ("effect", 200), ("either", 200), ("elem", 200), ("else", 240), ("email", 200), ("embed", 200),
    ("embedding", 200), ("empty", 240), ("enable", 240), ("encode", 200), ("encoder", 200), ("end", 240),
    ("engine", 200), ("enough", 200), ("ensure", 200), ("enter", 240), ("entry", 200), ("enum", 200),
    ("env", 240), ("eof", 200), ("epoch", 200), ("equal", 200), ("err", 200), ("errno", 200),
    ("error", 240), ("esc", 200), ("escape", 200), ("eval", 240), ("even", 160), ("event", 240),
    ("every", 160), ("exact", 200), ("exam", 200), ("excel", 200), ("except", 200), ("exec", 200),
    ("exist", 200), ("exit", 240), ("expand", 200), ("expect", 200), ("expert", 200), ("expire", 200),
    ("export", 240), ("expr", 200), ("ext", 200), ("extend", 200), ("extra", 200), ("eye", 200),
    ("face", 200), ("fact", 200), ("factor", 200), ("fail", 200), ("false", 240), ("family", 200),
    ("far", 200), ("fast", 240), ("fatal", 160), ("favor", 80), ("fax", 80), ("feature", 200),
    ("feed", 200), ("feel", 200), ("fetch", 200), ("few", 200), ("fft", 80), ("field", 200),
    ("fig", 200), ("figma", 160), ("file", 240), ("files", 240), ("fill", 200), ("filter", 240),
    ("final", 200), ("find", 240), ("fine", 200), ("finetune", 200), ("finish", 200), ("fire", 200),
    ("first", 240), ("fit", 200), ("fix", 240), ("fixed", 200), ("flag", 200), ("flags", 200),
    ("flash", 200), ("flat", 200), ("flex", 200), ("flip", 200), ("float", 240), ("floor", 200),
    ("flow", 200), ("flush", 200), ("fly", 200), ("fmt", 200), ("focus", 200), ("fold", 200),
    ("folder", 200), ("follow", 200), ("font", 200), ("foo", 200), ("food", 200), ("foot", 200),
    ("for", 240), ("force", 200), ("fork", 200), ("form", 240), ("format", 240), ("forward", 200),
    ("found", 200), ("fp16", 160), ("fp8", 160), ("frame", 200), ("free", 200), ("fresh", 200),
    ("friend", 200), ("from", 240), ("front", 200), ("ftp", 200), ("full", 240), ("fun", 200),
    ("func", 240), ("function", 240), ("future", 200), ("gain", 200), ("game", 200), ("gap", 200),
    ("gas", 80), ("gate", 200), ("gcc", 200), ("gdb", 200), ("gem", 160), ("gemini", 200),
    ("gemm", 200), ("gen", 200), ("general", 200), ("get", 240), ("gguf", 120), ("git", 240),
    ("github", 240), ("gitignore", 160), ("gitlab", 120), ("give", 200), ("glad", 200), ("glass", 200),
    ("glibc", 80), ("global", 200), ("glow", 200), ("gnu", 200), ("go", 240), ("goal", 200),
    ("golang", 160), ("gold", 200), ("good", 240), ("google", 200), ("gpt", 240), ("gpu", 240),
    ("grab", 200), ("grace", 80), ("grade", 200), ("graph", 200), ("graphql", 160), ("gray", 200),
    ("great", 200), ("green", 200), ("grep", 200), ("grid", 200), ("ground", 200), ("group", 240),
    ("grow", 200), ("grpc", 160), ("guard", 200), ("guess", 200), ("guest", 80), ("gui", 200),
    ("guid", 200), ("guide", 200), ("gzip", 200), ("hack", 200), ("half", 200), ("halt", 200),
    ("hand", 200), ("handle", 200), ("hang", 200), ("happy", 200), ("hard", 200), ("hash", 240),
    ("have", 240), ("head", 240), ("header", 200), ("health", 200), ("heap", 200), ("heart", 200),
    ("heat", 200), ("heavy", 200), ("height", 200), ("hello", 240), ("help", 240), ("here", 240),
    ("hex", 200), ("hidden", 200), ("hide", 200), ("high", 240), ("hint", 200), ("hist", 200),
    ("history", 200), ("hit", 200), ("hold", 200), ("home", 240), ("hook", 200), ("hop", 80),
    ("hope", 200), ("host", 240), ("hot", 200), ("hotkey", 200), ("hour", 200), ("house", 200),
    ("hover", 200), ("how", 240), ("html", 240), ("http", 240), ("https", 240), ("hub", 200),
    ("huge", 200), ("huggingface", 160), ("human", 200), ("icon", 200), ("id", 240), ("idea", 200),
    ("ideal", 200), ("ident", 200), ("idle", 200), ("if", 240), ("ignore", 200), ("image", 240),
    ("img", 200), ("impl", 240), ("import", 240), ("in", 240), ("inbox", 160), ("inc", 200),
    ("inch", 80), ("include", 200), ("index", 240), ("info", 240), ("init", 240), ("inline", 200),
    ("inner", 200), ("input", 240), ("insert", 200), ("inside", 200), ("install", 240), ("int", 240),
    ("int8", 160), ("intent", 200), ("inter", 200), ("into", 240), ("intro", 200), ("io", 240),
    ("ip", 240), ("ipc", 200), ("is", 240), ("iso", 200), ("issue", 240), ("it", 240),
    ("item", 240), ("items", 160), ("iter", 200), ("its", 200), ("java", 200), ("javascript", 200),
    ("job", 200), ("jobs", 200), ("join", 200), ("json", 240), ("jsonl", 200), ("jsx", 200),
    ("jump", 200), ("just", 240), ("jvm", 200), ("jwt", 160), ("k8s", 160), ("keep", 240),
    ("kernel", 200), ("key", 240), ("keys", 240), ("kill", 200), ("kind", 200), ("kit", 200),
    ("know", 240), ("known", 200), ("kubernetes", 160), ("lab", 200), ("label", 200), ("lang", 200),
    ("large", 200), ("last", 240), ("late", 200), ("latency", 200), ("later", 200), ("launch", 200),
    ("layer", 200), ("layout", 200), ("lazy", 200), ("lead", 200), ("leaf", 200), ("leak", 200),
    ("learn", 200), ("least", 200), ("leave", 200), ("left", 240), ("len", 240), ("length", 200),
    ("less", 200), ("let", 240), ("letter", 200), ("level", 200), ("lib", 200), ("life", 200),
    ("lift", 200), ("light", 200), ("like", 240), ("limit", 240), ("line", 240), ("link", 240),
    ("linux", 240), ("list", 240), ("listen", 200), ("lite", 200), ("live", 200), ("llama", 160),
    ("llm", 240), ("load", 240), ("local", 240), ("localhost", 200), ("lock", 200), ("log", 240),
    ("logic", 200), ("login", 200), ("logo", 200), ("logs", 200), ("long", 240), ("look", 240),
    ("lookup", 200), ("loop", 240), ("lora", 160), ("loss", 200), ("lost", 200), ("lot", 200),
    ("loud", 160), ("love", 200), ("low", 240), ("lower", 200), ("lru", 200), ("lua", 200),
    ("mac", 200), ("macos", 200), ("macro", 200), ("magic", 200), ("mail", 200), ("main", 240),
    ("major", 200), ("make", 240), ("makefile", 160), ("man", 240), ("manage", 200), ("manual", 200),
    ("many", 240), ("map", 240), ("margin", 200), ("mark", 200), ("markdown", 200), ("mask", 200),
    ("master", 200), ("match", 240), ("math", 200), ("matrix", 200), ("max", 240), ("maybe", 200),
    ("mcp", 200), ("mean", 240), ("media", 200), ("medium", 200), ("meet", 200), ("mem", 240),
    ("member", 200), ("memory", 240), ("menu", 240), ("merge", 240), ("mermaid", 120), ("mesh", 160),
    ("meta", 200), ("metal", 160), ("method", 200), ("metric", 200), ("mid", 200), ("middle", 200),
    ("might", 200), ("min", 240), ("mind", 200), ("mini", 200), ("minus", 200), ("minute", 200),
    ("mirror", 200), ("miss", 200), ("mix", 200), ("mkdir", 200), ("mod", 200), ("mode", 240),
    ("model", 240), ("models", 240), ("modern", 200), ("modify", 200), ("module", 200), ("mon", 160),
    ("money", 200), ("mongodb", 120), ("monitor", 200), ("month", 200), ("more", 240), ("most", 240),
    ("mount", 200), ("mouse", 200), ("move", 240), ("msg", 240), ("msvc", 200), ("much", 240),
    ("multi", 200), ("music", 200), ("must", 240), ("mut", 240), ("mutex", 200), ("mypy", 120),
    ("name", 240), ("nano", 200), ("native", 200), ("nav", 200), ("near", 200), ("need", 240),
    ("neg", 200), ("nest", 200), ("net", 240), ("network", 200), ("never", 200), ("new", 240),
    ("news", 200), ("next", 240), ("nextjs", 160), ("nginx", 160), ("nice", 200), ("night", 200),
    ("nil", 200), ("no", 240), ("node", 240), ("nodes", 200), ("noise", 200), ("none", 240),
    ("norm", 200), ("normal", 200), ("north", 80), ("not", 240), ("note", 240), ("notes", 200),
    ("notice", 200), ("notion", 160), ("now", 240), ("npu", 200), ("null", 240), ("num", 240),
    ("number", 240), ("numpy", 200), ("nvidia", 200), ("oauth", 160), ("obj", 240), ("object", 240),
    ("odd", 200), ("of", 240), ("off", 240), ("offer", 200), ("office", 200), ("often", 200),
    ("ok", 240), ("okay", 200), ("old", 240), ("on", 240), ("once", 240), ("one", 240),
    ("only", 240), ("onnx", 160), ("open", 240), ("openai", 200), ("ops", 200), ("opt", 240),
    ("option", 240), ("or", 240), ("order", 240), ("org", 200), ("origin", 200), ("os", 240),
    ("other", 240), ("our", 240), ("out", 240), ("outer", 200), ("output", 240), ("over", 240),
    ("own", 240), ("owner", 200), ("pack", 200), ("package", 200), ("pad", 200), ("page", 240),
    ("paint", 200), ("pair", 200), ("pal", 80), ("panel", 200), ("panic", 200), ("paper", 200),
    ("param", 240), ("parent", 200), ("parse", 240), ("parser", 200), ("part", 240), ("pass", 240),
    ("paste", 200), ("patch", 200), ("path", 240), ("pause", 200), ("pay", 200), ("pdf", 200),
    ("peak", 200), ("peer", 200), ("pen", 80), ("pending", 200), ("per", 200), ("percent", 200),
    ("perf", 200), ("period", 200), ("perm", 200), ("phase", 200), ("phone", 200), ("photo", 200),
    ("php", 200), ("pic", 200), ("pick", 200), ("pid", 200), ("pill", 200), ("pin", 200),
    ("ping", 200), ("pip", 200), ("pipe", 200), ("pixel", 200), ("place", 200), ("plain", 200),
    ("plan", 240), ("play", 240), ("please", 120), ("plot", 200), ("plugin", 200), ("plus", 200),
    ("png", 200), ("pnpm", 160), ("point", 240), ("poll", 200), ("pool", 200), ("pop", 200),
    ("port", 240), ("pos", 200), ("post", 240), ("postgres", 160), ("pow", 200), ("power", 200),
    ("powershell", 160), ("pre", 240), ("prefix", 200), ("press", 200), ("pretrain", 160), ("pretty", 200),
    ("prev", 200), ("preview", 200), ("price", 200), ("prime", 200), ("print", 240), ("prior", 200),
    ("prisma", 120), ("priv", 200), ("pro", 240), ("probe", 200), ("proc", 200), ("process", 240),
    ("prod", 200), ("profile", 240), ("prog", 200), ("program", 200), ("proj", 200), ("project", 240),
    ("prompt", 240), ("proof", 200), ("prop", 200), ("props", 200), ("proto", 200), ("proxy", 200),
    ("prune", 200), ("ptr", 200), ("pub", 240), ("public", 240), ("pull", 240), ("pump", 200),
    ("pure", 200), ("push", 240), ("put", 240), ("pwd", 200), ("pwsh", 160), ("pytest", 160),
    ("python", 240), ("pytorch", 200), ("qemu", 200), ("query", 240), ("queue", 240), ("quick", 200),
    ("quit", 240), ("quiz", 160), ("quote", 200), ("qwen", 200), ("race", 200), ("rad", 80),
    ("radio", 200), ("rag", 200), ("rand", 200), ("random", 200), ("range", 200), ("rank", 200),
    ("rare", 200), ("rate", 200), ("ratio", 200), ("raw", 200), ("ray", 200), ("reach", 200),
    ("react", 200), ("read", 240), ("readme", 240), ("ready", 240), ("real", 240), ("reason", 200),
    ("reboot", 200), ("rec", 200), ("record", 200), ("rect", 200), ("red", 200), ("redis", 200),
    ("redo", 200), ("reduce", 200), ("ref", 240), ("refresh", 200), ("reg", 200), ("regex", 200),
    ("region", 200), ("reject", 200), ("rel", 200), ("release", 240), ("reload", 200), ("remain", 200),
    ("remote", 240), ("remove", 240), ("rename", 200), ("render", 240), ("rep", 200), ("repeat", 200),
    ("reply", 200), ("repo", 240), ("report", 200), ("req", 240), ("request", 240), ("require", 120),
    ("rerank", 200), ("reranker", 160), ("reset", 240), ("resize", 240), ("resolve", 240), ("resp", 200),
    ("rest", 200), ("restart", 200), ("result", 240), ("ret", 240), ("retry", 160), ("return", 240),
    ("rev", 200), ("review", 240), ("rich", 200), ("right", 240), ("ring", 200), ("role", 200),
    ("roll", 200), ("room", 200), ("root", 240), ("rope", 200), ("round", 200), ("route", 240),
    ("router", 200), ("row", 240), ("rows", 200), ("rpc", 200), ("rss", 200), ("ruff", 120),
    ("rule", 240), ("rules", 200), ("run", 240), ("runner", 200), ("rust", 240), ("rustc", 200),
    ("safe", 200), ("safetensors", 160), ("salt", 200), ("same", 240), ("sample", 200), ("sandbox", 200),
    ("save", 240), ("say", 240), ("scale", 240), ("scan", 240), ("scene", 200), ("schema", 240),
    ("scheme", 120), ("scoop", 120), ("scope", 200), ("score", 240), ("screen", 240), ("script", 240),
    ("scroll", 200), ("sdk", 240), ("search", 240), ("sec", 240), ("second", 240), ("secret", 200),
    ("section", 200), ("secure", 200), ("seed", 200), ("seek", 200), ("seem", 200), ("select", 240),
    ("self", 240), ("send", 240), ("sense", 200), ("sent", 200), ("seq", 200), ("serde", 160),
    ("serial", 200), ("serve", 200), ("server", 240), ("service", 240), ("session", 240), ("set", 240),
    ("sets", 200), ("setup", 240), ("seven", 200), ("sha", 200), ("shadow", 200), ("shape", 200),
    ("share", 240), ("sharp", 160), ("she", 160), ("sheet", 200), ("shell", 240), ("shift", 240),
    ("ship", 200), ("short", 240), ("should", 240), ("show", 240), ("shrink", 200), ("side", 240),
    ("sign", 240), ("signal", 200), ("silu", 200), ("simd", 200), ("simple", 240), ("since", 200),
    ("single", 200), ("sink", 200), ("site", 200), ("size", 240), ("skip", 240), ("slack", 160),
    ("sleep", 240), ("slice", 200), ("slide", 200), ("slot", 200), ("slow", 200), ("small", 200),
    ("smart", 200), ("smoke", 200), ("smooth", 200), ("snap", 200), ("so", 200), ("socket", 200),
    ("soft", 200), ("solid", 200), ("solve", 200), ("some", 240), ("soon", 200), ("sort", 240),
    ("sound", 200), ("source", 240), ("south", 80), ("space", 240), ("span", 200), ("sparse", 200),
    ("spawn", 200), ("spec", 240), ("speed", 200), ("spell", 200), ("spin", 200), ("split", 200),
    ("spring", 200), ("sql", 200), ("sqlite", 200), ("src", 240), ("ssh", 240), ("ssl", 160),
    ("stable", 200), ("stack", 200), ("stackoverflow", 160), ("stage", 200), ("stand", 200), ("start", 240),
    ("stat", 240), ("state", 240), ("static", 240), ("stats", 160), ("status", 240), ("stay", 200),
    ("std", 240), ("stdout", 200), ("step", 240), ("steps", 200), ("stick", 200), ("still", 200),
    ("stop", 240), ("store", 240), ("story", 200), ("str", 240), ("stream", 240), ("stride", 200),
    ("string", 240), ("strip", 200), ("struct", 240), ("stub", 200), ("studio", 200), ("style", 240),
    ("sub", 240), ("submit", 240), ("sudo", 240), ("sum", 240), ("supabase", 120), ("super", 200),
    ("support", 240), ("sure", 240), ("svg", 200), ("swap", 200), ("sweep", 200), ("sweet", 200),
    ("swift", 200), ("swing", 80), ("switch", 240), ("sym", 200), ("symbol", 200), ("sync", 240),
    ("syntax", 200), ("sys", 240), ("system", 240), ("tab", 240), ("table", 240), ("tabs", 200),
    ("tag", 240), ("tags", 200), ("tail", 200), ("tailwind", 160), ("take", 240), ("talk", 200),
    ("taobao", 160), ("tar", 200), ("target", 240), ("task", 240), ("tasks", 200), ("tauri", 120),
    ("tcp", 200), ("team", 240), ("tech", 200), ("tell", 200), ("temp", 240), ("template", 200),
    ("tensor", 200), ("term", 200), ("test", 240), ("tests", 240), ("text", 240), ("than", 240),
    ("thank", 240), ("thanks", 240), ("that", 240), ("the", 240), ("their", 240), ("them", 240),
    ("theme", 200), ("then", 240), ("there", 240), ("these", 240), ("they", 240), ("thin", 200),
    ("thing", 240), ("think", 240), ("this", 240), ("those", 240), ("thread", 240), ("three", 200),
    ("throughput", 160), ("throw", 200), ("thumb", 200), ("tick", 200), ("tidy", 200), ("tie", 200),
    ("tile", 200), ("till", 200), ("time", 240), ("timer", 200), ("tiny", 200), ("tip", 200),
    ("title", 240), ("tmp", 240), ("to", 240), ("todo", 240), ("toggle", 200), ("token", 240),
    ("tokenizer", 160), ("tokio", 160), ("toml", 200), ("tone", 200), ("too", 200), ("tool", 240),
    ("tools", 240), ("top", 240), ("topic", 200), ("total", 240), ("touch", 200), ("trace", 200),
    ("track", 240), ("train", 240), ("trait", 200), ("trans", 200), ("transformer", 200), ("tree", 240),
    ("trend", 200), ("trie", 200), ("trigger", 200), ("trim", 200), ("triton", 120), ("true", 240),
    ("trunk", 200), ("trust", 200), ("try", 240), ("tsf", 200), ("tsx", 200), ("tty", 200),
    ("tune", 200), ("tuple", 200), ("turn", 240), ("twice", 200), ("two", 240), ("txt", 240),
    ("type", 240), ("types", 240), ("typescript", 200), ("ubuntu", 200), ("udp", 200), ("ui", 240),
    ("uid", 200), ("uint", 200), ("ultra", 200), ("umask", 80), ("undef", 80), ("under", 200),
    ("undo", 240), ("unfold", 200), ("uni", 200), ("union", 200), ("unique", 200), ("unit", 240),
    ("unix", 200), ("unk", 200), ("unless", 200), ("unlink", 200), ("unlock", 200), ("unpack", 200),
    ("unset", 200), ("until", 200), ("up", 240), ("update", 240), ("upload", 240), ("upon", 200),
    ("upper", 200), ("uri", 200), ("url", 240), ("us", 240), ("usage", 200), ("usb", 200),
    ("use", 240), ("used", 240), ("user", 240), ("users", 240), ("using", 240), ("utf", 240),
    ("util", 240), ("utils", 240), ("uuid", 200), ("val", 240), ("valid", 240), ("value", 240),
    ("values", 240), ("var", 240), ("vars", 160), ("vec", 240), ("vector", 240), ("vendor", 200),
    ("ver", 240), ("vercel", 160), ("verify", 240), ("version", 240), ("versus", 200), ("very", 240),
    ("via", 240), ("video", 200), ("view", 240), ("views", 200), ("vim", 200), ("virt", 200),
    ("virtual", 200), ("visual", 200), ("vite", 200), ("vllm", 160), ("vnni", 120), ("void", 240),
    ("vol", 200), ("volume", 200), ("vscode", 200), ("vue", 200), ("wait", 240), ("wake", 200),
    ("walk", 200), ("wall", 200), ("want", 240), ("warm", 200), ("warn", 240), ("warning", 240),
    ("was", 240), ("wasm", 160), ("watch", 240), ("water", 160), ("wave", 200), ("way", 240),
    ("ways", 200), ("we", 240), ("weak", 200), ("web", 240), ("webhook", 200), ("webpack", 120),
    ("websocket", 160), ("wechat", 200), ("week", 240), ("weight", 200), ("welcome", 200), ("well", 240),
    ("were", 240), ("west", 80), ("what", 240), ("wheel", 80), ("when", 240), ("where", 240),
    ("which", 240), ("while", 240), ("white", 200), ("who", 240), ("whole", 200), ("whom", 80),
    ("whose", 200), ("why", 240), ("wide", 200), ("widget", 200), ("width", 240), ("wifi", 200),
    ("wikipedia", 160), ("win", 240), ("win32", 200), ("window", 240), ("windows", 240), ("windsurf", 120),
    ("winget", 120), ("wipe", 200), ("wire", 200), ("with", 240), ("within", 200), ("without", 200),
    ("word", 240), ("words", 240), ("work", 240), ("worker", 240), ("world", 240), ("worth", 200),
    ("would", 240), ("wrap", 200), ("write", 240), ("writer", 200), ("wrong", 240), ("wsl", 160),
    ("www", 200), ("x64", 200), ("x86", 200), ("xml", 240), ("xor", 200), ("yaml", 240),
    ("yarn", 200), ("year", 240), ("yes", 240), ("yet", 240), ("yield", 200), ("yml", 200),
    ("you", 240), ("young", 200), ("your", 240), ("youtube", 200), ("zero", 240), ("zig", 160),
    ("zip", 240), ("zone", 200), ("zoom", 200), ("zsh", 200),
];

/// Returns the exact typed `raw_keys` as candidate #1, optionally followed by frequency-ranked,
/// case-matched prefix completions from `ENGLISH_WORDS` (up to `max_results`).
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
    let start = ENGLISH_WORDS.partition_point(|&(w, _)| w < lower.as_str());
    let mut matches: Vec<(&str, u8)> = Vec::new();
    for &(word, freq) in &ENGLISH_WORDS[start..] {
        if !word.starts_with(lower.as_str()) {
            break;
        }
        if word != lower {
            matches.push((word, freq));
        }
    }
    matches.sort_by_key(|&(word, freq)| (Reverse(freq), word.len(), word));
    for (word, _) in matches {
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
                pair[0].0 < pair[1].0,
                "ENGLISH_WORDS must be strictly sorted & unique: {:?} vs {:?}",
                pair[0],
                pair[1]
            );
            assert!(pair[0].0.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
            assert!((50..=250).contains(&pair[0].1));
        }
        let last = ENGLISH_WORDS.last().unwrap();
        assert!(last.0.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert!((50..=250).contains(&last.1));
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

    #[test]
    fn suggest_english_candidates_prefers_higher_frequency_terms() {
        let co = suggest_english_candidates("co", 12);
        let code_pos = co.iter().position(|w| w == "code").unwrap();
        let config_pos = co.iter().position(|w| w == "config").unwrap();
        let commit_pos = co.iter().position(|w| w == "commit").unwrap();
        let codex_pos = co.iter().position(|w| w == "codex").unwrap_or(usize::MAX);
        assert!(
            code_pos <= 2 && commit_pos < codex_pos && config_pos < codex_pos,
            "got {co:?}"
        );

        let mo = suggest_english_candidates("mo", 10);
        let mode_pos = mo.iter().position(|w| w == "mode").unwrap();
        let model_pos = mo.iter().position(|w| w == "model").unwrap();
        let mod_pos = mo.iter().position(|w| w == "mod").unwrap_or(usize::MAX);
        assert!(mode_pos < mod_pos && model_pos < mod_pos, "got {mo:?}");

        let py = suggest_english_candidates("py", 5);
        assert_eq!(py.get(1).map(String::as_str), Some("python"));
    }
}
