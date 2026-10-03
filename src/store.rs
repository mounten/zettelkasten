//! Storage: a vault is a folder of Markdown files, one per card, with YAML
//! front matter. App-wide preferences (recent vaults, layout) live in the
//! platform data dir.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Card, CardColor, CardKind, TodoItem, fenced};

/// Hidden folder inside a vault for app state and the trash.
const META_DIR: &str = ".zettelkasten";
const MAX_RECENT: usize = 8;

// ----- app config ---------------------------------------------------------------

/// App-wide preferences, stored in `%APPDATA%\Zettelkasten\config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub recent_vaults: Vec<PathBuf>,
    pub last_vault: Option<PathBuf>,
    pub sidebar_width: f32,
    pub dock_width: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            recent_vaults: Vec::new(),
            last_vault: None,
            sidebar_width: 232.,
            dock_width: 280.,
        }
    }
}

impl AppConfig {
    /// The config dir; override with the `ZETTELKASTEN_DIR` environment variable.
    pub fn dir() -> PathBuf {
        std::env::var_os("ZETTELKASTEN_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("Zettelkasten")))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn load() -> Self {
        fs::read_to_string(Self::dir().join("config.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        write_atomic(&Self::dir().join("config.json"), json.as_bytes())
    }

    /// Move `path` to the top of the recent vaults and make it the last one.
    pub fn remember(&mut self, path: &Path) {
        self.recent_vaults.retain(|p| p != path);
        self.recent_vaults.insert(0, path.to_path_buf());
        self.recent_vaults.truncate(MAX_RECENT);
        self.last_vault = Some(path.to_path_buf());
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent_vaults.retain(|p| p != path);
        if self.last_vault.as_deref() == Some(path) {
            self.last_vault = None;
        }
    }
}

// ----- legacy store ---------------------------------------------------------------

/// The single `cards.json` used before vaults existed.
pub fn legacy_cards_path() -> PathBuf {
    AppConfig::dir().join("cards.json")
}

#[derive(Deserialize)]
struct LegacyFile {
    cards: Vec<Card>,
}

pub fn load_legacy_cards() -> Option<Vec<Card>> {
    let text = fs::read_to_string(legacy_cards_path()).ok()?;
    serde_json::from_str::<LegacyFile>(&text).ok().map(|f| f.cards)
}

/// Keep the old file around, but out of the way.
pub fn retire_legacy_cards() -> io::Result<()> {
    let path = legacy_cards_path();
    fs::rename(&path, path.with_extension("json.migrated"))
}

// ----- vault ------------------------------------------------------------------------

/// Per-vault UI state, stored in `<vault>/.zettelkasten/state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultState {
    /// Ids of minimized cards, most recent first.
    pub minimized: Vec<String>,
}

pub struct Vault {
    root: PathBuf,
    /// Card id → file in the vault.
    files: HashMap<String, PathBuf>,
}

pub struct LoadedVault {
    pub vault: Vault,
    pub cards: Vec<Card>,
    /// Files that could not be read, with the reason.
    pub errors: Vec<String>,
}

impl Vault {
    /// Open (or create) a vault in `root` and load all cards in it.
    pub fn open(root: &Path) -> io::Result<LoadedVault> {
        fs::create_dir_all(root.join(META_DIR))?;
        let mut vault = Vault {
            root: root.to_path_buf(),
            files: HashMap::new(),
        };
        let mut cards = Vec::new();
        let mut errors = Vec::new();

        let mut paths = Vec::new();
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") && path.is_file() {
                paths.push(path);
            }
        }

        // Reading is dominated by per-file overhead (and virus scanning on a
        // cold cache), so spread it over all cores.
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = paths.len().div_ceil(threads).max(1);
        let results: Vec<(PathBuf, io::Result<Card>)> = std::thread::scope(|scope| {
            let workers: Vec<_> = paths
                .chunks(chunk)
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|path| (path.clone(), read_card(path)))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_default())
                .collect()
        });

        for (path, result) in results {
            match result {
                Ok(card) => {
                    if vault.files.contains_key(&card.id) {
                        errors.push(format!("{}: duplicate card id {}", file_name(&path), card.id));
                        continue;
                    }
                    vault.files.insert(card.id.clone(), path);
                    cards.push(card);
                }
                Err(err) => errors.push(format!("{}: {err}", file_name(&path))),
            }
        }
        cards.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(LoadedVault {
            vault,
            cards,
            errors,
        })
    }

    pub fn contains(&self, id: &str) -> bool {
        self.files.contains_key(id)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn name(&self) -> String {
        vault_name(&self.root)
    }

    /// Write one card to its Markdown file.
    pub fn save_card(&mut self, card: &Card) -> io::Result<()> {
        let path = match self.files.get(&card.id) {
            Some(path) => path.clone(),
            None => {
                let path = self.new_file_path(card);
                self.files.insert(card.id.clone(), path.clone());
                path
            }
        };
        write_atomic(&path, card_to_markdown(card).as_bytes())
    }

    /// Move a card's file into the vault trash.
    pub fn delete_card(&mut self, id: &str) -> io::Result<()> {
        let Some(path) = self.files.remove(id) else {
            return Ok(());
        };
        let trash = self.root.join(META_DIR).join("trash");
        fs::create_dir_all(&trash)?;
        fs::rename(&path, trash.join(file_name(&path)))
    }

    pub fn load_state(&self) -> VaultState {
        fs::read_to_string(self.root.join(META_DIR).join("state.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, state: &VaultState) -> io::Result<()> {
        let json = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
        write_atomic(&self.root.join(META_DIR).join("state.json"), json.as_bytes())
    }

    /// Zettelkasten style id from the creation time, e.g. `20261003143012.md`.
    fn new_file_path(&self, card: &Card) -> PathBuf {
        let stamp = card
            .created
            .with_timezone(&chrono::Local)
            .format("%Y%m%d%H%M%S")
            .to_string();
        let taken = |p: &PathBuf| p.exists() || self.files.values().any(|f| f == p);
        let mut path = self.root.join(format!("{stamp}.md"));
        let mut n = 1;
        while taken(&path) {
            n += 1;
            path = self.root.join(format!("{stamp}-{n}.md"));
        }
        path
    }
}

pub fn vault_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

// ----- markdown format ------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct FrontMatter {
    id: String,
    kind: CardKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default)]
    color: CardColor,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    created: DateTime<Utc>,
    updated: DateTime<Utc>,
}

pub fn card_to_markdown(card: &Card) -> String {
    let front = FrontMatter {
        id: card.id.clone(),
        kind: card.kind,
        title: card.title.clone(),
        tags: card.tags.clone(),
        color: card.color,
        pinned: card.pinned,
        language: card.language.clone(),
        created: card.created,
        updated: card.updated,
    };
    let yaml = serde_yaml::to_string(&front).unwrap_or_default();
    let body = match card.kind {
        CardKind::Note => card.body.clone(),
        CardKind::Snippet => fenced(&card.body, card.language()),
        CardKind::Todo => {
            let mut lines: Vec<String> = card
                .items
                .iter()
                .map(|i| format!("- [{}] {}", if i.done { "x" } else { " " }, i.text))
                .collect();
            if !card.body.trim().is_empty() {
                lines.push(String::new());
                lines.push(card.body.trim().to_string());
            }
            lines.join("\n")
        }
    };
    format!("---\n{yaml}---\n\n{body}\n")
}

fn read_card(path: &Path) -> io::Result<Card> {
    let text = fs::read_to_string(path)?;
    let text = text.replace("\r\n", "\n");
    let Some((yaml, body)) = split_front_matter(&text) else {
        return Ok(plain_markdown_card(path, &text));
    };
    let front: FrontMatter = match serde_yaml::from_str(yaml) {
        Ok(front) => front,
        // Front matter from another app (e.g. Obsidian): treat as a note.
        Err(_) if !yaml.contains("kind:") => return Ok(plain_markdown_card(path, &text)),
        Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
    };
    Ok(card_from_parts(front, body))
}

fn card_from_parts(front: FrontMatter, body: &str) -> Card {
    let body = body.strip_prefix('\n').unwrap_or(body);
    let body = body.strip_suffix('\n').unwrap_or(body);
    let (body, items) = match front.kind {
        CardKind::Note => (body.to_string(), Vec::new()),
        CardKind::Snippet => (unfence(body).to_string(), Vec::new()),
        CardKind::Todo => {
            let mut items = Vec::new();
            let mut rest = Vec::new();
            for line in body.lines() {
                match parse_task(line) {
                    Some(item) => items.push(item),
                    None => rest.push(line),
                }
            }
            (rest.join("\n").trim().to_string(), items)
        }
    };
    Card {
        id: front.id,
        kind: front.kind,
        title: front.title,
        body,
        items,
        tags: front.tags,
        color: front.color,
        pinned: front.pinned,
        language: front.language,
        created: front.created,
        updated: front.updated,
    }
}

/// A Markdown file without our front matter becomes a note; saving it later
/// adds front matter but keeps the file and its text.
fn plain_markdown_card(path: &Path, text: &str) -> Card {
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::now());
    let modified: DateTime<Utc> = modified.into();
    let mut card = Card::new(CardKind::Note);
    card.id = format!("file:{}", file_name(path));
    card.title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    card.body = text.trim_end().to_string();
    card.created = modified;
    card.updated = modified;
    card
}

fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let (yaml_end, body_start) = match rest.find("\n---\n") {
        Some(i) => (i + 1, i + 5),
        None => {
            let r = rest.strip_suffix("\n---")?;
            (r.len() + 1, rest.len())
        }
    };
    Some((&rest[..yaml_end], &rest[body_start..]))
}

/// Strip a surrounding code fence, if the whole body is one.
fn unfence(body: &str) -> &str {
    let trimmed = body.trim();
    let Some(first_end) = trimmed.find('\n') else {
        return body;
    };
    let first = &trimmed[..first_end];
    let ticks = first.chars().take_while(|c| *c == '`').count();
    if ticks < 3 {
        return body;
    }
    let fence = &first[..ticks];
    let inner = &trimmed[first_end + 1..];
    match inner.rfind('\n') {
        Some(i) if inner[i + 1..].trim() == fence => &inner[..i],
        None if inner.trim() == fence => "",
        _ => body,
    }
}

fn parse_task(line: &str) -> Option<TodoItem> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(card: &Card) -> Card {
        let text = card_to_markdown(card);
        let (yaml, body) = split_front_matter(&text).expect("front matter");
        card_from_parts(serde_yaml::from_str(yaml).unwrap(), body)
    }

    #[test]
    fn note_round_trip() {
        let mut card = Card::new(CardKind::Note);
        card.title = "Idea: \"quotes\" and: colons".into();
        card.body = "# Heading\n\n---\n\nSome **bold** text".into();
        card.tags = vec!["a".into(), "b-c".into()];
        card.pinned = true;
        let back = round_trip(&card);
        assert_eq!(back.title, card.title);
        assert_eq!(back.body, card.body);
        assert_eq!(back.tags, card.tags);
        assert!(back.pinned);
    }

    #[test]
    fn todo_round_trip() {
        let mut card = Card::new(CardKind::Todo);
        card.items = vec![
            TodoItem { text: "milk".into(), done: true },
            TodoItem { text: "eggs".into(), done: false },
        ];
        let back = round_trip(&card);
        assert_eq!(back.items, card.items);
        assert!(back.body.is_empty());
    }

    #[test]
    fn snippet_round_trip() {
        let mut card = Card::new(CardKind::Snippet);
        card.body = "let s = \"```\";\nprintln!(\"{s}\");".into();
        card.language = Some("rust".into());
        let back = round_trip(&card);
        assert_eq!(back.body, card.body);
        assert_eq!(back.language.as_deref(), Some("rust"));
    }

    #[test]
    fn vault_files() {
        let dir = std::env::temp_dir().join(format!("zk-test-{}", uuid::Uuid::new_v4()));
        let mut card = Card::new(CardKind::Note);
        card.body = "hello".into();
        {
            let mut loaded = Vault::open(&dir).unwrap();
            loaded.vault.save_card(&card).unwrap();
        }
        fs::write(dir.join("Plain.md"), "Just text").unwrap();
        let mut loaded = Vault::open(&dir).unwrap();
        assert_eq!(loaded.cards.len(), 2);
        assert!(loaded.errors.is_empty());
        let plain = loaded.cards.iter().find(|c| c.title == "Plain").unwrap();
        assert_eq!(plain.body, "Just text");
        loaded.vault.delete_card(&card.id).unwrap();
        assert_eq!(Vault::open(&dir).unwrap().cards.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod bench {
    /// Run with `ZK_BENCH_VAULT=<dir> cargo test --release load_vault_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn load_vault_timing() {
        let dir = std::env::var("ZK_BENCH_VAULT").expect("ZK_BENCH_VAULT");
        for run in 1..=2 {
            let start = std::time::Instant::now();
            let loaded = super::Vault::open(std::path::Path::new(&dir)).unwrap();
            println!(
                "run {run}: {} cards, {} errors in {:?}",
                loaded.cards.len(),
                loaded.errors.len(),
                start.elapsed()
            );
        }
    }
}
