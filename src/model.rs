use chrono::{DateTime, Datelike as _, Local, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardKind {
    Note,
    Todo,
    Snippet,
}

impl CardKind {
    pub const ALL: [CardKind; 3] = [CardKind::Note, CardKind::Todo, CardKind::Snippet];

    pub fn label(self) -> &'static str {
        match self {
            CardKind::Note => "Note",
            CardKind::Todo => "Todo",
            CardKind::Snippet => "Snippet",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            CardKind::Note => "Notes",
            CardKind::Todo => "Todos",
            CardKind::Snippet => "Snippets",
        }
    }
}

/// Post-it style accent color of a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardColor {
    #[default]
    Slate,
    Yellow,
    Orange,
    Rose,
    Violet,
    Blue,
    Emerald,
}

impl CardColor {
    pub const ALL: [CardColor; 7] = [
        CardColor::Slate,
        CardColor::Yellow,
        CardColor::Orange,
        CardColor::Rose,
        CardColor::Violet,
        CardColor::Blue,
        CardColor::Emerald,
    ];

    /// Hex value of the accent.
    pub fn hex(self) -> u32 {
        match self {
            CardColor::Slate => 0x71717a,
            CardColor::Yellow => 0xfacc15,
            CardColor::Orange => 0xfb923c,
            CardColor::Rose => 0xfb7185,
            CardColor::Violet => 0xa78bfa,
            CardColor::Blue => 0x60a5fa,
            CardColor::Emerald => 0x34d399,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub kind: CardKind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub items: Vec<TodoItem>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub color: CardColor,
    #[serde(default)]
    pub pinned: bool,
    /// Snippet language; `None` means auto-detect from the content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

impl Card {
    pub fn new(kind: CardKind) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            title: String::new(),
            body: String::new(),
            items: Vec::new(),
            tags: Vec::new(),
            color: match kind {
                CardKind::Todo => CardColor::Yellow,
                _ => CardColor::Slate,
            },
            pinned: false,
            language: None,
            created: now,
            updated: now,
        }
    }

    pub fn touch(&mut self) {
        self.updated = Utc::now();
    }

    /// The title, falling back to the first line of the content.
    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        let first = match self.kind {
            CardKind::Todo => self.items.first().map(|i| i.text.as_str()),
            CardKind::Note => self
                .body
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().trim_start_matches('#').trim_start()),
            // The code itself is shown on the card, don't repeat it as title.
            CardKind::Snippet => None,
        };
        match first {
            Some(line) => truncate(line.trim(), 60),
            None if self.kind == CardKind::Snippet && !self.body.trim().is_empty() => {
                match self.body.lines().count() {
                    1 => "Snippet".into(),
                    n => format!("Snippet · {n} lines"),
                }
            }
            None => format!("Untitled {}", self.kind.label().to_lowercase()),
        }
    }

    pub fn has_title(&self) -> bool {
        !self.title.trim().is_empty()
    }

    /// Language used to highlight a snippet.
    pub fn language(&self) -> &str {
        match &self.language {
            Some(lang) => lang,
            None => detect_language(&self.body),
        }
    }

    /// A todo card with at least one unfinished task.
    pub fn is_open_todo(&self) -> bool {
        self.kind == CardKind::Todo && self.items.iter().any(|i| !i.done)
    }

    /// A todo card where every task is done.
    pub fn is_done_todo(&self) -> bool {
        self.kind == CardKind::Todo && !self.items.is_empty() && self.items.iter().all(|i| i.done)
    }

    pub fn progress(&self) -> (usize, usize) {
        let done = self.items.iter().filter(|i| i.done).count();
        (done, self.items.len())
    }

    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.body.trim().is_empty() && self.items.is_empty()
    }

    /// Convert the card to another kind, carrying the content over.
    pub fn convert(&mut self, kind: CardKind) {
        if self.kind == kind {
            return;
        }
        if kind == CardKind::Todo && self.items.is_empty() {
            self.items = self
                .body
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(|l| {
                    let (done, text) = strip_checkbox(l);
                    TodoItem {
                        text: text.to_string(),
                        done,
                    }
                })
                .collect();
            self.body.clear();
        } else if self.kind == CardKind::Todo && self.body.trim().is_empty() {
            self.body = self
                .items
                .iter()
                .map(|i| format!("- [{}] {}", if i.done { "x" } else { " " }, i.text))
                .collect::<Vec<_>>()
                .join("\n");
            self.items.clear();
        }
        self.kind = kind;
        self.touch();
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }

    /// Whether the card matches a search query.
    ///
    /// The query is split by whitespace and every term must match. `#tag`
    /// matches tags by prefix, `is:note|todo|snippet|pinned|open|done` filters
    /// on state, and any other term matches title, body, items or tags.
    pub fn matches(&self, query: &str) -> bool {
        query.split_whitespace().all(|term| {
            let term = term.to_lowercase();
            if let Some(tag) = term.strip_prefix('#') {
                return self.tags.iter().any(|t| t.to_lowercase().starts_with(tag));
            }
            if let Some(filter) = term.strip_prefix("is:") {
                return match filter {
                    "note" | "notes" => self.kind == CardKind::Note,
                    "todo" | "todos" => self.kind == CardKind::Todo,
                    "snippet" | "snippets" => self.kind == CardKind::Snippet,
                    "pinned" => self.pinned,
                    "open" => self.is_open_todo(),
                    "done" => self.is_done_todo(),
                    _ => false,
                };
            }
            if let Some(lang) = term.strip_prefix("lang:") {
                return self.kind == CardKind::Snippet && self.language().starts_with(lang);
            }
            self.title.to_lowercase().contains(&term)
                || self.body.to_lowercase().contains(&term)
                || self.items.iter().any(|i| i.text.to_lowercase().contains(&term))
                || self.tags.iter().any(|t| t.to_lowercase().contains(&term))
        })
    }
}

/// Parse `- [x] text`, `[ ] text`, `- text` style lines.
fn strip_checkbox(line: &str) -> (bool, &str) {
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line);
    for (prefix, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return (done, rest);
        }
    }
    (false, line)
}

/// Normalize user tag input: strip `#`, trim, lowercase, spaces become dashes.
pub fn normalize_tag(raw: &str) -> Option<String> {
    let tag = raw
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    (!tag.is_empty()).then_some(tag)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Human friendly relative time, e.g. "5m ago".
pub fn relative_time(time: DateTime<Utc>) -> String {
    let secs = (Utc::now() - time).num_seconds().max(0);
    match secs {
        0..=44 => "just now".into(),
        45..=3599 => format!("{}m ago", (secs / 60).max(1)),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        86_400..=604_799 => format!("{}d ago", secs / 86_400),
        _ => time.with_timezone(&Local).format("%b %e, %Y").to_string(),
    }
}

/// Languages offered for snippets: (tree-sitter name, label).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("text", "Plain text"),
    ("bash", "Bash"),
    ("c", "C"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("css", "CSS"),
    ("go", "Go"),
    ("html", "HTML"),
    ("java", "Java"),
    ("javascript", "JavaScript"),
    ("json", "JSON"),
    ("lua", "Lua"),
    ("markdown", "Markdown"),
    ("python", "Python"),
    ("rust", "Rust"),
    ("sql", "SQL"),
    ("toml", "TOML"),
    ("tsx", "TSX"),
    ("typescript", "TypeScript"),
    ("yaml", "YAML"),
];

pub fn language_label(name: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, label)| *label)
        .unwrap_or(name)
}

/// Best-effort guess of a snippet's language from its content.
pub fn detect_language(code: &str) -> &'static str {
    let text = code.trim();
    if text.is_empty() {
        return "text";
    }
    let has = |needle: &str| text.contains(needle);
    let starts = |prefix: &str| text.lines().any(|l| l.trim_start().starts_with(prefix));
    let lower = text.to_lowercase();

    if (text.starts_with('{') || text.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return "json";
    }
    if text.starts_with("#!") && (has("bash") || has("/sh")) {
        return "bash";
    }
    if has("fn ") && (has("let ") || has("->") || has("::") || has("pub ") || has("impl ")) {
        return "rust";
    }
    let cpp_hints = has("std::")
        || has("cout")
        || has("nullptr")
        || starts("template")
        || starts("namespace ")
        || starts("using namespace")
        || (has("class ") && (has("public:") || has("private:")));
    if starts("#include") || starts("#pragma") || starts("#define") {
        return if cpp_hints || has("class ") { "cpp" } else { "c" };
    }
    if cpp_hints {
        return "cpp";
    }
    if starts("package main") || (has("func ") && has(":=")) {
        return "go";
    }
    if has("using System") || (has("namespace ") && has("public ")) {
        return "csharp";
    }
    if has("public class ") || has("System.out.") || has("public static void main") {
        return "java";
    }
    if starts("def ") || (starts("import ") && !has(";") && !has(" from '")) || starts("from ") && has(" import ") {
        return "python";
    }
    if starts("<!doctype") || lower.starts_with("<!doctype") || starts("<html") || starts("<div") {
        return "html";
    }
    if ["select ", "insert into", "create table", "update ", "delete from"]
        .iter()
        .any(|kw| lower.starts_with(kw))
    {
        return "sql";
    }
    if (has("import ") || has("const ") || has("function ") || has("=>"))
        && (has("<") && has("/>"))
    {
        return "tsx";
    }
    if has("interface ") || has(": string") || has(": number") || has("export type ") {
        return "typescript";
    }
    if has("const ") || has("function ") || has("=>") || has("console.log") || has("let ") && has(";") {
        return "javascript";
    }
    if starts("local ") || (has("function ") && has(" end")) {
        return "lua";
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.iter().any(|l| l.starts_with('[') && l.trim_end().ends_with(']'))
        && lines.iter().any(|l| l.contains(" = "))
    {
        return "toml";
    }
    if has("{") && has("}") && has(":") && has(";") && !has("(") {
        return "css";
    }
    if lines.len() > 1
        && lines.iter().all(|l| {
            let t = l.trim_start();
            t.starts_with("- ") || t.starts_with('#') || t.contains(": ") || t.ends_with(':')
        })
    {
        return "yaml";
    }
    let shell = ["git ", "cargo ", "npm ", "cd ", "sudo ", "ls", "echo ", "curl ", "docker ", "$ "];
    if lines.iter().all(|l| shell.iter().any(|c| l.trim_start().starts_with(c)) || l.trim_start().starts_with('#')) {
        return "bash";
    }
    "text"
}

/// Wrap code in a fence that cannot collide with backticks inside it.
pub fn fenced(code: &str, language: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for ch in code.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{language}
{code}
{fence}")
}

/// Section label for the day a card was created.
pub fn day_label(date: chrono::NaiveDate) -> String {
    let today = Local::now().date_naive();
    match (today - date).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => date.format("%A").to_string(),
        _ if date.year_ce() == today.year_ce() => date.format("%A, %B %-d").to_string(),
        _ => date.format("%B %-d, %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_terms() {
        let mut card = Card::new(CardKind::Note);
        card.title = "Rust ownership".into();
        card.body = "Borrowing rules".into();
        card.tags = vec!["rust".into(), "learning".into()];
        assert!(card.matches("rust borrow"));
        assert!(card.matches("#lea"));
        assert!(card.matches("is:note #rust"));
        assert!(!card.matches("is:todo"));
        assert!(!card.matches("python"));
    }

    #[test]
    fn convert_round_trip() {
        let mut card = Card::new(CardKind::Note);
        card.body = "- [x] milk\n- [ ] eggs\n\nbread".into();
        card.convert(CardKind::Todo);
        assert_eq!(card.items.len(), 3);
        assert!(card.items[0].done);
        assert_eq!(card.items[2].text, "bread");
        card.convert(CardKind::Note);
        assert_eq!(card.body, "- [x] milk\n- [ ] eggs\n- [ ] bread");
    }

    #[test]
    fn detects_languages() {
        assert_eq!(detect_language("fn main() {
    let x = 1;
}"), "rust");
        assert_eq!(detect_language("def foo():
    return 1"), "python");
        assert_eq!(detect_language("git log --oneline"), "bash");
        assert_eq!(detect_language("{\"a\": 1}"), "json");
        assert_eq!(detect_language("SELECT * FROM cards;"), "sql");
        assert_eq!(detect_language("const x = () => 1;"), "javascript");
        assert_eq!(detect_language("just some words"), "text");
        assert_eq!(detect_language("#include <vector>
std::vector<int> v;"), "cpp");
        assert_eq!(detect_language("template <typename T>
T max(T a, T b) { return a > b ? a : b; }"), "cpp");
        assert_eq!(detect_language("#include <stdio.h>
int main(void) { return 0; }"), "c");
    }

    #[test]
    fn markdown_heading_title() {
        let mut card = Card::new(CardKind::Note);
        card.body = "# Big idea
body".into();
        assert_eq!(card.display_title(), "Big idea");
    }

    #[test]
    fn tags_normalize() {
        assert_eq!(normalize_tag(" #Deep Work "), Some("deep-work".into()));
        assert_eq!(normalize_tag("#"), None);
    }
}
