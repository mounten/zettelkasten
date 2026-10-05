use std::ops::Range;

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
    /// Project the card belongs to, e.g. `zettelkasten`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Component within the project, e.g. `sidebar`; only set with a project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// Feature within the component, e.g. `saved-queries`; only set with a component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
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
            project: None,
            component: None,
            feature: None,
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
            CardKind::Todo => self.items.first().map(|i| i.text.clone()),
            CardKind::Note => self
                .body
                .lines()
                .map(plain_line)
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().trim_start_matches('#').trim_start().to_string()),
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
            if self.body.lines().any(|l| parse_task(l).is_some()) {
                // Task lines become tasks, the rest stays as the description.
                let mut rest = Vec::new();
                for line in self.body.lines() {
                    match parse_task(line) {
                        Some(item) => self.items.push(item),
                        None => rest.push(line),
                    }
                }
                self.body = rest.join("\n").trim().to_string();
            } else {
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
            }
        } else if self.kind == CardKind::Todo {
            self.body = self.todo_markdown();
            self.items.clear();
        }
        self.kind = kind;
        self.touch();
    }

    /// A todo card as markdown: the description, then the task list.
    pub fn todo_markdown(&self) -> String {
        let tasks = self
            .items
            .iter()
            .map(|i| format!("- [{}] {}", if i.done { "x" } else { " " }, i.text))
            .collect::<Vec<_>>()
            .join("\n");
        match self.body.trim() {
            "" => tasks,
            description if tasks.is_empty() => description.to_string(),
            description => format!("{description}\n\n{tasks}"),
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }

    pub fn in_project(&self, project: &str) -> bool {
        self.project.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(project))
    }

    pub fn in_component(&self, project: &str, component: &str) -> bool {
        self.in_project(project)
            && self.component.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(component))
    }

    pub fn in_feature(&self, project: &str, component: &str, feature: &str) -> bool {
        self.in_component(project, component)
            && self.feature.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(feature))
    }

    /// Set the project; changing it drops the component and feature, which belong to the old one.
    pub fn set_project(&mut self, project: Option<String>) {
        if self.project != project {
            self.project = project;
            self.component = None;
            self.feature = None;
        }
    }

    /// Set the component; changing it drops the feature. Needs a project.
    pub fn set_component(&mut self, component: Option<String>) {
        let component = component.filter(|_| self.project.is_some());
        if self.component != component {
            self.component = component;
            self.feature = None;
        }
    }

    /// Set the feature. Needs a component.
    pub fn set_feature(&mut self, feature: Option<String>) {
        self.feature = feature.filter(|_| self.component.is_some());
    }

    /// Whether the card matches a search query.
    ///
    /// The query is split by whitespace and every term must match. `#tag`
    /// matches tags by prefix, `tag:name`, `project:name`, `component:name` and
    /// `feature:name` match exactly (`project:none` finds cards without one),
    /// `is:note|todo|snippet|pinned|open|done` filters on state, and any other
    /// term matches title, body, items, tags, project, component or feature.
    pub fn matches(&self, query: &str) -> bool {
        query.split_whitespace().all(|term| {
            let term = term.to_lowercase();
            if let Some(tag) = term.strip_prefix('#') {
                return self.tags.iter().any(|t| t.to_lowercase().starts_with(tag));
            }
            if let Some(tag) = term.strip_prefix("tag:") {
                return self.has_tag(tag);
            }
            if let Some(project) = term.strip_prefix("project:") {
                return match project {
                    "none" => self.project.is_none(),
                    _ => self.in_project(project),
                };
            }
            if let Some(component) = term.strip_prefix("component:") {
                return match component {
                    "none" => self.component.is_none(),
                    _ => self.component.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(component)),
                };
            }
            if let Some(feature) = term.strip_prefix("feature:") {
                return match feature {
                    "none" => self.feature.is_none(),
                    _ => self.feature.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(feature)),
                };
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
                || self.project.as_deref().is_some_and(|p| p.contains(&term))
                || self.component.as_deref().is_some_and(|c| c.contains(&term))
                || self.feature.as_deref().is_some_and(|f| f.contains(&term))
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

/// Parse a `- [ ] text` or `* [x] text` task line.
pub fn parse_task(line: &str) -> Option<TodoItem> {
    let line = line.trim_start();
    let line = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))?;
    for (prefix, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
        if let Some(text) = line.strip_prefix(prefix) {
            return Some(TodoItem {
                text: text.to_string(),
                done,
            });
        }
    }
    None
}

/// An edit that replaces `range` (byte offsets) with `text`.
#[derive(Debug, PartialEq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// What Enter at `cursor` does in a markdown list: in an item it starts the
/// next one (`- `, `2. `, `- [ ] `), on an empty item it removes the marker to
/// end the list. `None` means an ordinary newline.
pub fn list_enter(text: &str, cursor: usize) -> Option<TextEdit> {
    let start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let end = text[cursor..].find('\n').map_or(text.len(), |i| cursor + i);
    // Lists inside code blocks are code.
    let fences = text[..start].lines().filter(|l| l.trim_start().starts_with("```")).count();
    if fences % 2 == 1 {
        return None;
    }
    let line = &text[start..end];
    let (marker_len, next) = list_marker(line)?;
    if cursor < start + marker_len {
        return None;
    }
    if line[marker_len..].trim().is_empty() {
        // A blank line after the list, or the next line would join the last item.
        let after_text = text[..start].lines().next_back().is_some_and(|l| !l.trim().is_empty());
        let text = if after_text { "\n" } else { "" };
        return Some(TextEdit { range: start..end, text: text.into() });
    }
    Some(TextEdit { range: cursor..cursor, text: format!("\n{next}") })
}

/// The length of the list marker a line starts with, and the marker for the next item.
fn list_marker(line: &str) -> Option<(usize, String)> {
    let rest = line.trim_start_matches([' ', '\t']);
    let indent = &line[..line.len() - rest.len()];
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let (len, mut next) = if let Some(bullet) = rest.chars().next().filter(|c| "-*+".contains(*c))
        && rest[1..].starts_with(' ')
    {
        (2, format!("{indent}{bullet} "))
    } else if (1..10).contains(&digits)
        && let Some(delim) = rest[digits..].chars().next().filter(|c| ".)".contains(*c))
        && rest[digits + 1..].starts_with(' ')
    {
        let n: u64 = rest[..digits].parse().ok()?;
        (digits + 2, format!("{indent}{}{delim} ", n + 1))
    } else {
        return None;
    };
    // A task item continues with an open task.
    let after = &rest[len..];
    let mut len = indent.len() + len;
    if let Some(checkbox) = ["[ ]", "[x]", "[X]"].into_iter().find(|b| after.starts_with(b))
        && matches!(after[checkbox.len()..].chars().next(), None | Some(' '))
    {
        len += (checkbox.len() + 1).min(after.len());
        next.push_str("[ ] ");
    }
    Some((len, next))
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
    let tag = tag.trim_matches('-').to_string();
    (!tag.is_empty()).then_some(tag)
}

/// Tag input as it is typed: spaces become single dashes, so `hello world`
/// turns into `hello-world` instead of two tags.
pub fn dash_spaces(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.trim_start().chars() {
        if ch.is_whitespace() {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// A named search kept in the sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub name: String,
    pub query: String,
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

/// Whether a file name or link points to an image the app can show inline.
pub fn is_image(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext.to_lowercase());
    matches!(
        ext.as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

/// Markdown that embeds an attachment: images inline, other files as a link.
pub fn embed_markdown(link: &str) -> String {
    let name = link.rsplit('/').next().unwrap_or(link);
    if is_image(link) {
        format!("![{name}]({link})")
    } else {
        format!("[{name}]({link})")
    }
}

/// An inline Markdown link or image, `[label](target)` or `![label](target)`.
#[derive(Debug, Clone, PartialEq)]
pub struct MdLink {
    pub image: bool,
    pub label: String,
    pub target: String,
    /// Byte range of the whole link in the text.
    pub range: std::ops::Range<usize>,
}

impl MdLink {
    /// Points to a file rather than a web page or an anchor.
    pub fn is_local(&self) -> bool {
        let t = &self.target;
        !(t.contains("://") || t.starts_with('#') || t.starts_with("mailto:") || t.starts_with("data:"))
    }

    /// The file name the link points to.
    pub fn file_name(&self) -> &str {
        self.target.rsplit(['/', '\\']).next().unwrap_or(&self.target)
    }
}

/// The inline links and images in Markdown text, in order.
pub fn md_links(text: &str) -> Vec<MdLink> {
    let mut links = Vec::new();
    let mut pos = 0;
    while let Some(open) = text[pos..].find('[').map(|i| pos + i) {
        let image = text[..open].ends_with('!');
        let after = &text[open + 1..];
        let parsed = after.find("](").and_then(|close| {
            let label = &after[..close];
            let target_start = open + 1 + close + 2;
            let end = text[target_start..].find(')')? + target_start;
            // A `[` in the label starts the next candidate.
            (!label.contains(['[', '\n'])).then_some((label, target_start, end))
        });
        let Some((label, target_start, end)) = parsed else {
            pos = open + 1;
            continue;
        };
        let target = text[target_start..end].trim();
        // Drop an optional `"title"` and `<…>` around the destination.
        let target = target.split(" \"").next().unwrap_or(target).trim_matches(['<', '>']);
        if !target.is_empty() && !target.contains('\n') {
            links.push(MdLink {
                image,
                label: label.to_string(),
                target: target.to_string(),
                range: if image { open - 1 } else { open }..end + 1,
            });
        }
        pos = end + 1;
    }
    links
}

/// The text with all images removed, e.g. for previews that show them apart.
pub fn strip_images(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for link in md_links(text).into_iter().filter(|l| l.image) {
        out.push_str(&text[last..link.range.start]);
        last = link.range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// A line as plain text: images dropped, links reduced to their label.
fn plain_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for link in md_links(line) {
        out.push_str(&line[last..link.range.start]);
        if !link.image {
            out.push_str(&link.label);
        }
        last = link.range.end;
    }
    out.push_str(&line[last..]);
    out
}

/// File size for display, e.g. "12 KB".
pub fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
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
        card.body = "Shopping\n\n- [x] milk\n- [ ] eggs".into();
        card.convert(CardKind::Todo);
        assert_eq!(card.items.len(), 2);
        assert!(card.items[0].done);
        assert_eq!(card.body, "Shopping");
        card.convert(CardKind::Note);
        assert_eq!(card.body, "Shopping\n\n- [x] milk\n- [ ] eggs");
        assert!(card.items.is_empty());

        // Without task lines every line becomes a task.
        let mut card = Card::new(CardKind::Note);
        card.body = "- milk\n\nbread".into();
        card.convert(CardKind::Todo);
        let texts: Vec<&str> = card.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["milk", "bread"]);
        assert!(card.body.is_empty());
    }

    fn enter(text: &str) -> String {
        let cursor = text.find('|').unwrap();
        let mut text = text.replace('|', "");
        if let Some(edit) = list_enter(&text, cursor) {
            text.replace_range(edit.range, &edit.text);
        }
        text
    }

    #[test]
    fn lists_continue_on_enter() {
        assert_eq!(enter("- milk|"), "- milk\n- ");
        assert_eq!(enter("a\n  * milk|\nb"), "a\n  * milk\n  * \nb");
        assert_eq!(enter("9. nine|"), "9. nine\n10. ");
        assert_eq!(enter("1) one|"), "1) one\n2) ");
        assert_eq!(enter("- [x] done|"), "- [x] done\n- [ ] ");
        assert_eq!(enter("- mi|lk"), "- mi\n- lk");
        // An empty item ends the list.
        assert_eq!(enter("- milk\n- |"), "- milk\n\n");
        assert_eq!(enter("- milk\n- [ ] |\nmore"), "- milk\n\n\nmore");
        assert_eq!(enter("- milk\n- [ ]|"), "- milk\n\n");
        assert_eq!(enter("- |"), "");
        assert_eq!(enter("text\n\n- |"), "text\n\n");
        // Not in a list, or before the marker.
        assert_eq!(enter("text|"), "text");
        assert_eq!(enter("-no space|"), "-no space");
        assert_eq!(enter("|- milk"), "- milk");
        assert_eq!(enter("```\n- code|"), "```\n- code");
        assert_eq!(enter("```\n```\n- milk|"), "```\n```\n- milk\n- ");
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
    fn project_terms() {
        let mut card = Card::new(CardKind::Todo);
        card.project = Some("zettel".into());
        card.component = Some("sidebar".into());
        card.tags = vec!["ui".into()];
        assert!(card.matches("project:zettel"));
        assert!(!card.matches("project:zet"));
        assert!(card.matches("project:Zettel component:sidebar"));
        assert!(!card.matches("component:none"));
        assert!(card.matches("tag:ui sidebar"));
        assert!(!card.matches("tag:u"));
        card.set_project(Some("other".into()));
        assert_eq!(card.component, None);
        assert!(card.matches("component:none"));
        card.set_component(Some("store".into()));
        card.set_feature(Some("yaml".into()));
        assert!(card.matches("feature:yaml"));
        assert!(card.in_feature("other", "store", "yaml"));
        card.set_component(Some("sidebar".into()));
        assert_eq!(card.feature, None);
        card.set_project(None);
        card.set_component(Some("sidebar".into()));
        assert_eq!(card.component, None);
    }

    #[test]
    fn embeds() {
        assert_eq!(embed_markdown("attachments/a.PNG"), "![a.PNG](attachments/a.PNG)");
        assert_eq!(embed_markdown("attachments/doc.pdf"), "[doc.pdf](attachments/doc.pdf)");
        let text = "See [doc.pdf](attachments/doc.pdf) and ![pic](attachments/p.png),\n\
                    [site](https://x.org), [[wiki]] and [b](<c.zip> \"title\").";
        let links = md_links(text);
        let summary: Vec<(bool, &str, &str, bool)> = links
            .iter()
            .map(|l| (l.image, l.label.as_str(), l.target.as_str(), l.is_local()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (false, "doc.pdf", "attachments/doc.pdf", true),
                (true, "pic", "attachments/p.png", true),
                (false, "site", "https://x.org", false),
                (false, "b", "c.zip", true),
            ]
        );
        assert_eq!(&text[links[1].range.clone()], "![pic](attachments/p.png)");
        assert_eq!(links[0].file_name(), "doc.pdf");
        assert_eq!(strip_images("a ![x](y.png) b"), "a  b");

        let mut card = Card::new(CardKind::Note);
        card.body = "![shot](attachments/s.png)\n[report.pdf](attachments/report.pdf) to read".into();
        assert_eq!(card.display_title(), "report.pdf to read");
    }

    #[test]
    fn tags_normalize() {
        assert_eq!(normalize_tag(" #Deep Work "), Some("deep-work".into()));
        assert_eq!(normalize_tag("#"), None);
        assert_eq!(normalize_tag("hello-"), Some("hello".into()));
        assert_eq!(dash_spaces(" hello  world "), "hello-world-");
        assert_eq!(dash_spaces("hello- world"), "hello-world");
    }
}
