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

use crate::model::{Card, CardColor, CardKind, SavedQuery, fenced, parse_task};

/// Hidden folder inside a vault for app state and the trash.
const META_DIR: &str = ".zettelkasten";
/// Folder inside a vault for embedded images and files.
const ATTACHMENTS_DIR: &str = "attachments";
const MAX_RECENT: usize = 8;

// ----- app config ---------------------------------------------------------------

/// Color theme; `System` follows the OS light/dark setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    Light,
    #[default]
    Dark,
    System,
}

impl ThemePreference {
    pub const ALL: [ThemePreference; 3] =
        [ThemePreference::Light, ThemePreference::Dark, ThemePreference::System];

    pub fn label(self) -> &'static str {
        match self {
            ThemePreference::Light => "Light",
            ThemePreference::Dark => "Dark",
            ThemePreference::System => "System",
        }
    }
}

/// App-wide preferences, stored in `%APPDATA%\Zettelkasten\config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub recent_vaults: Vec<PathBuf>,
    pub last_vault: Option<PathBuf>,
    pub sidebar_width: f32,
    pub dock_width: f32,
    /// Collapsed sidebar sections, by label.
    pub collapsed_sections: Vec<String>,
    pub theme: ThemePreference,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            recent_vaults: Vec::new(),
            last_vault: None,
            sidebar_width: 232.,
            dock_width: 280.,
            collapsed_sections: Vec::new(),
            theme: ThemePreference::default(),
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
    /// Searches saved in the sidebar, in display order.
    pub queries: Vec<SavedQuery>,
    /// Projects whose components are shown in the sidebar.
    pub expanded_projects: Vec<String>,
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

    /// Copy a file into the attachment folder and return its vault-relative
    /// path for a Markdown link. Files already inside the vault are linked
    /// where they are.
    pub fn import_attachment(&self, source: &Path) -> io::Result<String> {
        if let Some(rel) = self.relative_path(source) {
            return Ok(rel);
        }
        let name = file_name(source);
        let target = self.new_attachment_path(&name)?;
        fs::copy(source, &target)?;
        Ok(self.relative_path(&target).unwrap_or_default())
    }

    /// Store raw bytes, e.g. a pasted image, as an attachment.
    pub fn save_attachment(&self, name: &str, bytes: &[u8]) -> io::Result<String> {
        let target = self.new_attachment_path(name)?;
        fs::write(&target, bytes)?;
        Ok(self.relative_path(&target).unwrap_or_default())
    }

    /// A free path in the attachment folder for a file called `name`.
    fn new_attachment_path(&self, name: &str) -> io::Result<PathBuf> {
        let dir = self.root.join(ATTACHMENTS_DIR);
        fs::create_dir_all(&dir)?;
        let (stem, ext) = link_safe_name(name);
        let mut path = dir.join(format!("{stem}{ext}"));
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("{stem}-{n}{ext}"));
        }
        Ok(path)
    }

    /// `path` relative to the vault with forward slashes, if it is inside it
    /// and can be written into a Markdown link as is.
    fn relative_path(&self, path: &Path) -> Option<String> {
        let root = self.root.canonicalize().ok()?;
        let path = path.canonicalize().ok()?;
        let parts: Vec<String> = path
            .strip_prefix(&root)
            .ok()?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        parts
            .iter()
            .all(|p| {
                let (stem, ext) = link_safe_name(p);
                format!("{stem}{ext}") == *p
            })
            .then(|| parts.join("/"))
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

/// Split a file name into a stem and `.ext` that need no escaping in a
/// Markdown link: anything but letters, digits, `-` and `_` becomes a dash.
fn link_safe_name(name: &str) -> (String, String) {
    let clean = |s: &str| {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                out.push(ch);
            } else if !out.ends_with('-') {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    };
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (clean(stem), clean(ext).to_lowercase()),
        _ => (clean(name), String::new()),
    };
    let stem = if stem.is_empty() { "file".to_string() } else { stem };
    let ext = if ext.is_empty() { ext } else { format!(".{ext}") };
    (stem, ext)
}

/// The local file a Markdown link points to: a path relative to the vault
/// root or an absolute one; `None` for URLs. Does not touch the disk.
pub fn link_path(root: &Path, link: &str) -> Option<PathBuf> {
    let link = link.trim().trim_start_matches('<').trim_end_matches('>');
    let link = link.strip_prefix("file:///").unwrap_or(link);
    // `C:/...` has a colon too, but no `//` after it.
    if link.is_empty()
        || link.starts_with('#')
        || link.contains("://")
        || link.starts_with("data:")
        || link.starts_with("mailto:")
    {
        return None;
    }
    let link = percent_decode(link);
    let path = Path::new(&link);
    Some(if path.is_absolute() { path.to_path_buf() } else { root.join(path) })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feature: Option<String>,
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
        project: card.project.clone(),
        component: card.component.clone(),
        feature: card.feature.clone(),
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
        CardKind::Todo => card.todo_markdown(),
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
        // A component without a project, or a feature without a component,
        // has nowhere to show up.
        feature: front.feature.filter(|_| front.project.is_some() && front.component.is_some()),
        component: front.component.filter(|_| front.project.is_some()),
        project: front.project,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TodoItem;

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
        card.project = Some("zettel".into());
        card.component = Some("store".into());
        card.feature = Some("front-matter".into());
        card.pinned = true;
        let back = round_trip(&card);
        assert_eq!(back.title, card.title);
        assert_eq!(back.body, card.body);
        assert_eq!(back.tags, card.tags);
        assert_eq!(back.project, card.project);
        assert_eq!(back.component, card.component);
        assert_eq!(back.feature, card.feature);
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

        card.body = "Weekly **groceries**\n\n- not a task".into();
        let back = round_trip(&card);
        assert_eq!(back.items, card.items);
        assert_eq!(back.body, card.body);
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

    #[test]
    fn attachments() {
        let dir = std::env::temp_dir().join(format!("zk-test-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("zk-src-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&outside).unwrap();
        let source = outside.join("My Report (final).PDF");
        fs::write(&source, b"%PDF").unwrap();

        let vault = Vault::open(&dir).unwrap().vault;
        let first = vault.import_attachment(&source).unwrap();
        assert_eq!(first, "attachments/My-Report-final.pdf");
        assert_eq!(vault.import_attachment(&source).unwrap(), "attachments/My-Report-final-2.pdf");
        // Already in the vault: linked, not copied again.
        assert_eq!(vault.import_attachment(&dir.join(&first)).unwrap(), first);
        assert_eq!(
            vault.save_attachment("pasted image.png", b"png").unwrap(),
            "attachments/pasted-image.png"
        );
        assert!(Vault::open(&dir).unwrap().cards.is_empty());

        assert_eq!(link_path(&dir, &first), Some(dir.join(&first)));
        assert_eq!(link_path(&dir, "attachments/My%2DReport-final.pdf"), Some(dir.join(&first)));
        assert_eq!(link_path(&dir, "https://example.com/a.png"), None);
        assert_eq!(link_path(&dir, "#heading"), None);
        assert_eq!(link_path(&dir, &source.display().to_string()), Some(source.clone()));

        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
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
