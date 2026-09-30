//! Syntax highlighting for the code blocks in agent text: code and its
//! language in, lines of text runs tagged with what kind of token each is
//! out. The theme maps each kind to one of the terminal's own palette hues,
//! so code reads in the person's colours. The engine behind this interface
//! is syntect's grammars on a pure-Rust regex engine; another engine (a
//! tree-sitter one, say) can replace it without the chat changing.
//!
//! The grammars load once, on a background thread the first time a block
//! asks for them; until they arrive blocks draw in the reading ink and
//! [`loading`] asks the frame to come back. Each block is highlighted once
//! and kept; a block still streaming in resumes from its last whole line.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// What a run of code is, as far as colouring it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TokenKind {
    /// Names, operators and punctuation: the reading ink.
    Plain,
    Keyword,
    String,
    /// Numbers and other literal constants.
    Number,
    Type,
    Function,
    Comment,
    /// A diff's added and removed lines.
    Inserted,
    Deleted,
}

/// A block's lines, each a run of tagged text.
pub(crate) type Highlighted = Vec<Vec<(String, TokenKind)>>;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// Whether the grammars are on their way: a frame drawn now shows code
/// plain and should be drawn again soon.
pub(crate) fn loading() -> bool {
    STARTED.load(Ordering::Relaxed) && SYNTAXES.get().is_none()
}

/// Loads the grammars on this thread, for a caller that wants its first
/// frame highlighted and can wait for it. Returns how long it took.
pub fn preload() -> std::time::Duration {
    let started = std::time::Instant::now();
    STARTED.store(true, Ordering::Relaxed);
    let _ = SYNTAXES.get_or_init(load);
    started.elapsed()
}

/// How long the first block takes once the grammars are in: `code`
/// highlighted as `language`, for measuring.
pub fn time_block(code: &str, language: &str) -> std::time::Duration {
    let started = std::time::Instant::now();
    let _ = highlight(code, language);
    started.elapsed()
}

/// Whether blocks can be highlighted yet.
pub(crate) fn ready() -> bool {
    SYNTAXES.get().is_some()
}

/// The grammars, once loaded; the first ask starts loading them off the
/// draw thread.
fn syntaxes() -> Option<&'static SyntaxSet> {
    if let Some(set) = SYNTAXES.get() {
        return Some(set);
    }
    if !STARTED.swap(true, Ordering::Relaxed) {
        std::thread::spawn(|| {
            let _ = SYNTAXES.set(load());
        });
    }
    None
}

/// The grammars, with the languages agents write most already compiled: a
/// grammar compiles its patterns the first time it parses, which costs a
/// frame's worth of time, so it is paid here, off the draw thread.
fn load() -> SyntaxSet {
    let set = SyntaxSet::load_defaults_newlines();
    for (language, line) in [
        ("rust", "fn main() { let x = \"a\"; }\n"),
        ("bash", "for i in $(seq 3); do echo \"$i\"; done\n"),
        ("js", "const x = () => ({ a: 1 });\n"),
        ("python", "def f(x): return x\n"),
        ("json", "{\"a\": [1, true]}\n"),
        ("go", "func main() { fmt.Println(1) }\n"),
        ("yaml", "a: [1, 2]\n"),
        ("diff", "+a\n"),
    ] {
        if let Some(syntax) = grammar(&set, language) {
            let _ = ParseState::new(syntax).parse_line(line, &set);
        }
    }
    set
}

/// The grammar for a fence's language word, by name or file extension,
/// with the common words agents write that the grammars do not know.
fn grammar<'a>(set: &'a SyntaxSet, language: &str) -> Option<&'a SyntaxReference> {
    let word = language
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()?
        .trim()
        .to_ascii_lowercase();
    let word = match word.as_str() {
        "" => return None,
        "ts" | "tsx" | "typescript" | "jsx" | "mjs" | "cjs" => "js",
        "shell" | "console" | "zsh" | "sh" | "shellscript" => "bash",
        "py" | "python3" => "python",
        "yml" => "yaml",
        "golang" => "go",
        "c++" | "hpp" | "cc" => "cpp",
        "objc" | "objective-c" => "m",
        "patch" => "diff",
        "jsonc" | "json5" => "json",
        "md" => "markdown",
        other => other,
    };
    set.find_syntax_by_token(word)
}

struct Cache {
    /// Finished blocks, by language and code.
    blocks: HashMap<u64, Arc<Highlighted>>,
    /// The block last highlighted line by line: its whole lines so far and
    /// the parser's state after them, so a block streaming in only parses
    /// what arrived.
    stream: Option<Stream>,
}

struct Stream {
    language: String,
    lines: Vec<String>,
    state: ParseState,
    stack: ScopeStack,
    tokens: Highlighted,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            blocks: HashMap::new(),
            stream: None,
        })
    })
}

/// Highlights `code` written in `language`. None while the grammars are
/// loading or when the language is not known: the block then reads in the
/// reading ink.
pub(crate) fn highlight(code: &str, language: &str) -> Option<Arc<Highlighted>> {
    let set = syntaxes()?;
    let syntax = grammar(set, language)?;
    let key = {
        let mut hasher = DefaultHasher::new();
        syntax.name.hash(&mut hasher);
        code.hash(&mut hasher);
        hasher.finish()
    };
    let mut cache = cache().lock().ok()?;
    if let Some(done) = cache.blocks.get(&key) {
        return Some(done.clone());
    }
    // Whole lines carry on from the block streamed so far when they
    // extend it; a last line without its newline is parsed from a copy of
    // the state, so it is parsed again, whole, when it completes.
    let lines: Vec<&str> = code.split_inclusive('\n').collect();
    let whole = lines.iter().take_while(|line| line.ends_with('\n')).count();
    let resumable = cache.stream.as_ref().is_some_and(|stream| {
        stream.language == syntax.name
            && stream.lines.len() <= whole
            && stream.lines.iter().zip(&lines).all(|(a, b)| a == b)
    });
    let mut stream = match cache.stream.take() {
        Some(stream) if resumable => stream,
        _ => Stream {
            language: syntax.name.clone(),
            lines: Vec::new(),
            state: ParseState::new(syntax),
            stack: ScopeStack::new(),
            tokens: Vec::new(),
        },
    };
    let kinds = Kinds::get();
    for line in &lines[stream.lines.len()..whole] {
        let tokens = tokens_of(line, &mut stream.state, &mut stream.stack, set, kinds);
        stream.tokens.push(tokens);
        stream.lines.push((*line).to_owned());
    }
    let mut tokens = stream.tokens.clone();
    if let Some(tail) = lines.get(whole) {
        let (mut state, mut stack) = (stream.state.clone(), stream.stack.clone());
        tokens.push(tokens_of(tail, &mut state, &mut stack, set, kinds));
    }
    let tokens = Arc::new(tokens);
    // A finished block (no partial last line) is kept for good; the
    // stream carries on for the next extension either way.
    if whole == lines.len() {
        if cache.blocks.len() > 512 {
            cache.blocks.clear();
        }
        cache.blocks.insert(key, tokens.clone());
    }
    cache.stream = Some(stream);
    Some(tokens)
}

/// One line's runs, the parser moved past it.
fn tokens_of(
    line: &str,
    state: &mut ParseState,
    stack: &mut ScopeStack,
    set: &SyntaxSet,
    kinds: &Kinds,
) -> Vec<(String, TokenKind)> {
    let text = line.trim_end_matches(['\n', '\r']);
    let Ok(ops) = state.parse_line(line, set) else {
        return vec![(text.to_owned(), TokenKind::Plain)];
    };
    let mut out: Vec<(String, TokenKind)> = Vec::new();
    let mut at = 0;
    let add = |from: usize, to: usize, kind: TokenKind, out: &mut Vec<(String, TokenKind)>| {
        let to = to.min(text.len());
        if from >= to {
            return;
        }
        let piece = &text[from..to];
        match out.last_mut() {
            Some((last, last_kind)) if *last_kind == kind => last.push_str(piece),
            _ => out.push((piece.to_owned(), kind)),
        }
    };
    for (offset, op) in ops {
        add(at, offset, kinds.of(stack), &mut out);
        at = offset.max(at);
        let _ = stack.apply(&op);
    }
    add(at, text.len(), kinds.of(stack), &mut out);
    out
}

/// The scope prefixes that decide a token's kind, looked up from the
/// innermost scope outward: the first that matches wins.
struct Kinds {
    rules: Vec<(Scope, TokenKind)>,
}

impl Kinds {
    fn get() -> &'static Kinds {
        static KINDS: OnceLock<Kinds> = OnceLock::new();
        KINDS.get_or_init(|| {
            use TokenKind::*;
            let rules = [
                ("comment", Comment),
                ("string", String),
                ("markup.inserted", Inserted),
                ("markup.deleted", Deleted),
                ("constant.numeric", Number),
                ("constant.language", Number),
                ("constant.character", Number),
                ("constant.other", Number),
                ("entity.name.function", Function),
                ("support.function", Function),
                ("variable.function", Function),
                ("entity.name.type", Type),
                ("entity.name.class", Type),
                ("entity.name.struct", Type),
                ("entity.name.enum", Type),
                ("entity.name.trait", Type),
                ("entity.name.interface", Type),
                ("support.type", Type),
                ("support.class", Type),
                ("entity.other.inherited-class", Type),
                ("entity.other.attribute-name", Type),
                ("storage.type", Keyword),
                ("storage.modifier", Keyword),
                ("keyword.operator.word", Keyword),
                ("keyword.operator", Plain),
                ("keyword", Keyword),
                ("variable.language", Keyword),
                ("entity.name.tag", Keyword),
                ("markup.heading", Keyword),
                ("support.constant", Number),
                ("punctuation", Plain),
            ]
            .into_iter()
            .filter_map(|(name, kind)| Scope::new(name).ok().map(|scope| (scope, kind)))
            .collect();
            Kinds { rules }
        })
    }

    fn of(&self, stack: &ScopeStack) -> TokenKind {
        for scope in stack.as_slice().iter().rev() {
            // Punctuation inside a string or comment is part of it: the
            // quotes read in the string's colour.
            if let Some((_, kind)) = self
                .rules
                .iter()
                .find(|(prefix, _)| prefix.is_prefix_of(*scope))
                && *kind != TokenKind::Plain
            {
                return *kind;
            }
        }
        TokenKind::Plain
    }
}
