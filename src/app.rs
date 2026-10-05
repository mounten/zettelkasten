use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chrono::{Local, NaiveDate};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Colorize as _, Icon, InteractiveElementExt as _, Sizable as _,
    StyledExt as _, Theme, ThemeMode, TitleBar,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, Enter, Input, InputEvent, InputState, Paste},
    kbd::Kbd,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    text::{TextView, TextViewStyle},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::model::{
    Card, CardColor, CardKind, LANGUAGES, MdLink, SavedQuery, TodoItem, dash_spaces, day_label,
    detect_language, embed_markdown, fenced, human_size, is_image, language_label, list_enter,
    md_links, normalize_tag, relative_time, strip_images, truncate,
};
use crate::store::{
    AppConfig, LoadedVault, ThemePreference, Vault, VaultState, legacy_cards_path, link_path,
    load_legacy_cards, retire_legacy_cards, vault_name,
};

const CONTEXT: &str = "Zettelkasten";
const LIST_MAX_WIDTH: f32 = 860.;
const LIST_PADDING: f32 = 28.;
const HANDLE_WIDTH: Pixels = px(5.);
const SIDEBAR_RANGE: (f32, f32) = (180., 420.);
const DOCK_RANGE: (f32, f32) = (220., 480.);
const MODAL_MAX: (f32, f32) = (860., 820.);
const MAIN_MIN: f32 = 320.;
/// How many lines of a snippet the list shows.
const SNIPPET_PREVIEW_LINES: usize = 14;
const TODO_DESCRIPTION_HEIGHT: f32 = 120.;

actions!(
    zettelkasten,
    [
        NewNote,
        NewTodo,
        NewSnippet,
        PasteSnippet,
        FocusSearch,
        CloseEditor,
        DeleteCard,
        TogglePin,
        TogglePreview,
        MinimizeCard,
        OpenVault,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewNote, None),
        KeyBinding::new("secondary-t", NewTodo, None),
        KeyBinding::new("secondary-shift-n", NewSnippet, None),
        KeyBinding::new("secondary-shift-v", PasteSnippet, None),
        KeyBinding::new("secondary-k", FocusSearch, None),
        KeyBinding::new("secondary-f", FocusSearch, None),
        KeyBinding::new("secondary-p", TogglePin, None),
        KeyBinding::new("secondary-e", TogglePreview, None),
        KeyBinding::new("secondary-m", MinimizeCard, None),
        KeyBinding::new("secondary-o", OpenVault, None),
        KeyBinding::new("secondary-shift-backspace", DeleteCard, None),
        KeyBinding::new("escape", CloseEditor, Some(CONTEXT)),
    ]);
}

#[derive(Clone, PartialEq)]
enum Filter {
    All,
    Kind(CardKind),
    OpenTodos,
    Pinned,
    Tag(String),
    Project(String),
    /// A component within a project.
    Component(String, String),
    /// A feature within a project's component.
    Feature(String, String, String),
    Saved(SavedQuery),
}

impl Filter {
    fn accepts(&self, card: &Card) -> bool {
        match self {
            Filter::All => true,
            Filter::Kind(kind) => card.kind == *kind,
            Filter::OpenTodos => card.is_open_todo(),
            Filter::Pinned => card.pinned,
            Filter::Tag(tag) => card.has_tag(tag),
            Filter::Project(project) => card.in_project(project),
            Filter::Component(project, component) => card.in_component(project, component),
            Filter::Feature(project, component, feature) => {
                card.in_feature(project, component, feature)
            }
            Filter::Saved(saved) => card.matches(&saved.query),
        }
    }

    fn title(&self) -> String {
        match self {
            Filter::All => "All cards".into(),
            Filter::Kind(kind) => kind.plural().into(),
            Filter::OpenTodos => "Open todos".into(),
            Filter::Pinned => "Pinned".into(),
            Filter::Tag(tag) => format!("#{tag}"),
            Filter::Project(project) => project.clone(),
            Filter::Component(project, component) => format!("{project} › {component}"),
            Filter::Feature(project, component, feature) => {
                format!("{project} › {component} › {feature}")
            }
            Filter::Saved(saved) => saved.name.clone(),
        }
    }

    /// The same filter as search terms, so it can be saved as a query.
    fn as_query(&self) -> String {
        match self {
            Filter::All => String::new(),
            Filter::Kind(kind) => format!("is:{}", kind.label().to_lowercase()),
            Filter::OpenTodos => "is:open".into(),
            Filter::Pinned => "is:pinned".into(),
            Filter::Tag(tag) => format!("tag:{tag}"),
            Filter::Project(project) => format!("project:{project}"),
            Filter::Component(project, component) => {
                format!("project:{project} component:{component}")
            }
            Filter::Feature(project, component, feature) => {
                format!("project:{project} component:{component} feature:{feature}")
            }
            Filter::Saved(saved) => saved.query.clone(),
        }
    }
}

/// The sidebar form for creating or editing a saved query.
#[derive(Clone, Copy, PartialEq)]
struct QueryForm {
    /// The saved query being edited; `None` creates a new one.
    index: Option<usize>,
}

/// Sidebar sections that can be collapsed.
const SAVED_SECTION: &str = "SAVED";
const PROJECTS_SECTION: &str = "PROJECTS";
const TAGS_SECTION: &str = "TAGS";

/// Key of a component in the set of expanded sidebar entries.
fn component_key(project: &str, component: &str) -> String {
    format!("{project}/{component}")
}

#[derive(Clone, Copy, PartialEq)]
enum Resizing {
    Sidebar,
    Dock,
}

/// One entry of the virtualized card list.
#[derive(Clone, PartialEq)]
enum Row {
    /// Section separator: `None` is the pinned section, otherwise a creation day.
    Section { day: Option<NaiveDate>, count: usize },
    Card(String),
}

/// Sidebar counts, computed in one pass and cached.
#[derive(Default)]
struct Stats {
    all: usize,
    pinned: usize,
    open_todos: usize,
    kinds: [usize; 3],
    tags: BTreeMap<String, usize>,
    projects: BTreeMap<String, ProjectStats>,
    /// Saved query → matching cards.
    saved: HashMap<String, usize>,
}

#[derive(Default)]
struct ProjectStats {
    count: usize,
    components: BTreeMap<String, ComponentStats>,
}

#[derive(Default)]
struct ComponentStats {
    count: usize,
    features: BTreeMap<String, usize>,
}

impl Stats {
    fn compute(cards: &[Card], queries: &[SavedQuery]) -> Self {
        let mut stats = Stats {
            all: cards.len(),
            saved: queries.iter().map(|q| (q.query.clone(), 0)).collect(),
            ..Default::default()
        };
        for card in cards {
            stats.pinned += usize::from(card.pinned);
            stats.open_todos += usize::from(card.is_open_todo());
            stats.kinds[card.kind as usize] += 1;
            for tag in &card.tags {
                *stats.tags.entry(tag.clone()).or_insert(0) += 1;
            }
            if let Some(project) = &card.project {
                let entry = stats.projects.entry(project.clone()).or_default();
                entry.count += 1;
                if let Some(component) = &card.component {
                    let entry = entry.components.entry(component.clone()).or_default();
                    entry.count += 1;
                    if let Some(feature) = &card.feature {
                        *entry.features.entry(feature.clone()).or_insert(0) += 1;
                    }
                }
            }
            for (query, count) in stats.saved.iter_mut() {
                *count += usize::from(card.matches(query));
            }
        }
        stats
    }

    fn count(&self, filter: &Filter) -> usize {
        match filter {
            Filter::All => self.all,
            Filter::Kind(kind) => self.kinds[*kind as usize],
            Filter::OpenTodos => self.open_todos,
            Filter::Pinned => self.pinned,
            Filter::Tag(tag) => self.tags.get(tag).copied().unwrap_or(0),
            Filter::Project(project) => self.projects.get(project).map_or(0, |p| p.count),
            Filter::Component(project, component) => self
                .component(project, component)
                .map_or(0, |c| c.count),
            Filter::Feature(project, component, feature) => self
                .component(project, component)
                .and_then(|c| c.features.get(feature))
                .copied()
                .unwrap_or(0),
            Filter::Saved(saved) => self.saved.get(&saved.query).copied().unwrap_or(0),
        }
    }

    fn component(&self, project: &str, component: &str) -> Option<&ComponentStats> {
        self.projects.get(project)?.components.get(component)
    }
}

/// The parts of a card that the sidebar counts depend on.
#[derive(PartialEq)]
struct StatsKey {
    kind: CardKind,
    pinned: bool,
    open: bool,
    tags: Vec<String>,
    project: Option<String>,
    component: Option<String>,
    feature: Option<String>,
    /// Which saved queries the card matches.
    saved: Vec<bool>,
}

fn stats_key(card: &Card, queries: &[SavedQuery]) -> StatsKey {
    StatsKey {
        kind: card.kind,
        pinned: card.pinned,
        open: card.is_open_todo(),
        tags: card.tags.clone(),
        project: card.project.clone(),
        component: card.component.clone(),
        feature: card.feature.clone(),
        saved: queries.iter().map(|q| card.matches(&q.query)).collect(),
    }
}

/// A second click this soon after opening a card counts as a double click on it.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

/// Rows rendered beyond the viewport, so scrolling doesn't pop in.
const LIST_OVERDRAW: Pixels = px(800.);

/// Layout and dock state; layout is app-wide, the dock belongs to the vault.
struct UiSettings {
    sidebar_width: f32,
    dock_width: f32,
    /// Cards kept in the right sidebar, like editor tabs, in opening order.
    /// A card that is open but not in here is a preview.
    minimized: Vec<String>,
}

struct Undo {
    index: usize,
    card: Card,
    _dismiss: Task<()>,
}

pub struct ZettelApp {
    config: AppConfig,
    /// The open vault; `None` shows the welcome screen.
    vault: Option<Vault>,
    /// A vault being read in the background, with the task doing it.
    loading: Option<(PathBuf, Task<()>)>,
    cards: Vec<Card>,
    /// The visible list, recomputed when cards, filter or search change.
    rows: Vec<Row>,
    list_state: ListState,
    /// Number of card rows in `rows`.
    visible_count: usize,
    /// Card id → index in `cards`; rebuilt whenever `cards` is reordered.
    index: HashMap<String, usize>,
    stats: Stats,
    filter: Filter,
    query: String,
    /// Searches saved in the sidebar, belonging to the vault.
    queries: Vec<SavedQuery>,
    /// Projects (and `project/component`s) whose children the sidebar shows.
    expanded: HashSet<String>,
    /// Set while the sidebar shows the saved query form.
    query_form: Option<QueryForm>,
    selected: Option<String>,
    /// Show the rendered markdown instead of the source in the editor.
    preview: bool,
    /// When the open card was opened, to recognize the second click of a double click.
    opened_at: Option<Instant>,
    undo: Option<Undo>,
    ui: UiSettings,
    resizing: Option<Resizing>,
    focus_handle: FocusHandle,
    search: Entity<InputState>,
    title: Entity<InputState>,
    /// Shared body editor: markdown for notes, code for snippets.
    body: Entity<EditorState>,
    body_language: SharedString,
    tag_input: Entity<InputState>,
    project_input: Entity<InputState>,
    component_input: Entity<InputState>,
    feature_input: Entity<InputState>,
    item_input: Entity<InputState>,
    /// Fields of the saved query form.
    query_name: Entity<InputState>,
    query_text: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ZettelApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = AppConfig::load();
        let ui = UiSettings {
            sidebar_width: config.sidebar_width,
            dock_width: config.dock_width,
            minimized: Vec::new(),
        };

        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search cards…  #tag  project:name  is:open  lang:rust")
                .clean_on_escape()
        });
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Untitled"));
        let body = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .folding(false)
                .soft_wrap(true)
                .placeholder("Start writing… Markdown is supported")
        });
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add tag…"));
        let project_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project…"));
        let component_input = cx.new(|cx| InputState::new(window, cx).placeholder("Component…"));
        let feature_input = cx.new(|cx| InputState::new(window, cx).placeholder("Feature…"));
        let query_name = cx.new(|cx| InputState::new(window, cx).placeholder("Display name"));
        let query_text = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Query, e.g. project:app is:open")
        });
        let item_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add a task and press Enter…")
        });

        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if let InputEvent::Change = event {
                    this.query = state.read(cx).value().to_string();
                    this.refresh_rows(true, None);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&title, window, |this, state, event, _, cx| {
                if let InputEvent::Change = event {
                    let value = state.read(cx).value().to_string();
                    this.update_selected(cx, |card| card.title = value);
                }
            }),
            cx.subscribe_in(&body, window, |this, state, event, _, cx| {
                if let InputEvent::Change = event {
                    let value = state.read(cx).value().to_string();
                    this.update_selected(cx, |card| card.body = value);
                    // Auto-detected snippet languages follow the content.
                    if let Some(card) = this.selected_card()
                        && card.kind == CardKind::Snippet
                    {
                        let lang = card.language().to_string();
                        this.set_body_language(&lang, cx);
                    }
                    let _ = state;
                }
            }),
            cx.subscribe_in(&tag_input, window, |this, state, event, window, cx| {
                let value = state.read(cx).value().to_string();
                // Spaces join words (`hello world` → `hello-world`); a comma
                // or Enter finishes the tag.
                if let InputEvent::Change = event
                    && !value.contains(',')
                {
                    let dashed = dash_spaces(&value);
                    if dashed != value {
                        state.update(cx, |state, cx| state.set_value(dashed, window, cx));
                    }
                    return;
                }
                let commit = matches!(
                    event,
                    InputEvent::PressEnter { .. } | InputEvent::Blur | InputEvent::Change
                );
                if commit && !value.trim().is_empty() {
                    let tags: Vec<String> = value.split(',').filter_map(normalize_tag).collect();
                    this.update_selected(cx, |card| {
                        for tag in tags {
                            if !card.has_tag(&tag) {
                                card.tags.push(tag);
                            }
                        }
                    });
                    state.update(cx, |state, cx| state.set_value("", window, cx));
                }
            }),
            cx.subscribe_in(&project_input, window, |this, state, event, window, cx| {
                if let InputEvent::PressEnter { .. } | InputEvent::Blur = event {
                    let project = normalize_tag(&state.read(cx).value());
                    this.set_project(project, window, cx);
                }
            }),
            cx.subscribe_in(&component_input, window, |this, state, event, window, cx| {
                if let InputEvent::PressEnter { .. } | InputEvent::Blur = event {
                    let component = normalize_tag(&state.read(cx).value());
                    this.set_component(component, window, cx);
                }
            }),
            cx.subscribe_in(&feature_input, window, |this, state, event, window, cx| {
                if let InputEvent::PressEnter { .. } | InputEvent::Blur = event {
                    let feature = normalize_tag(&state.read(cx).value());
                    this.set_feature(feature, window, cx);
                }
            }),
            cx.subscribe_in(&query_name, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_query_form(window, cx);
                }
            }),
            cx.subscribe_in(&query_text, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_query_form(window, cx),
                // The form shows how many cards match.
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&item_input, window, |this, state, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let text = state.read(cx).value().trim().to_string();
                    if !text.is_empty() {
                        this.update_selected(cx, |card| {
                            card.items.push(TodoItem { text, done: false })
                        });
                        state.update(cx, |state, cx| state.set_value("", window, cx));
                    }
                }
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.config.theme == ThemePreference::System {
                    apply_theme(ThemePreference::System, window, cx);
                }
            }),
        ];
        apply_theme(config.theme, window, cx);

        let mut this = Self {
            config,
            vault: None,
            loading: None,
            cards: Vec::new(),
            rows: Vec::new(),
            list_state: ListState::new(0, ListAlignment::Top, LIST_OVERDRAW),
            visible_count: 0,
            index: HashMap::new(),
            stats: Stats::default(),
            filter: Filter::All,
            query: String::new(),
            queries: Vec::new(),
            expanded: HashSet::new(),
            query_form: None,
            selected: None,
            preview: false,
            opened_at: None,
            undo: None,
            ui,
            resizing: None,
            focus_handle: cx.focus_handle(),
            search,
            title,
            body,
            body_language: "markdown".into(),
            tag_input,
            project_input,
            component_input,
            feature_input,
            item_input,
            query_name,
            query_text,
            _subscriptions: subscriptions,
        };
        if let Some(path) = this.config.last_vault.clone().filter(|p| p.is_dir()) {
            this.open_vault(path, window, cx);
        }
        this.focus_handle.focus(window, cx);
        this
    }

    // ----- data -------------------------------------------------------------

    fn report(&self, message: String, window: Option<&mut Window>, cx: &mut Context<Self>) {
        match window {
            Some(window) => window.push_notification(Notification::error(message), cx),
            None => eprintln!("{message}"),
        }
    }

    /// Write one card to its file. Fresh empty cards are not written yet.
    fn persist(&mut self, id: &str, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let (Some(vault), Some(card)) = (self.vault.as_mut(), self.cards.iter().find(|c| c.id == id))
        else {
            return;
        };
        if card.is_empty() && !vault.contains(id) {
            return;
        }
        if let Err(err) = vault.save_card(card) {
            self.report(format!("Saving failed: {err}"), window, cx);
        }
    }

    /// Move a card's file to the vault trash.
    fn remove_file(&mut self, id: &str, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if let Some(vault) = self.vault.as_mut()
            && let Err(err) = vault.delete_card(id)
        {
            self.report(format!("Deleting failed: {err}"), window, cx);
        }
    }

    /// Newest first; the list groups by creation day.
    fn sort(&mut self) {
        self.cards.sort_by(|a, b| b.created.cmp(&a.created));
    }

    fn card(&self, id: &str) -> Option<&Card> {
        match self.index.get(id).and_then(|ix| self.cards.get(*ix)) {
            Some(card) if card.id == id => Some(card),
            // The index is stale only between a change and the next refresh.
            _ => self.cards.iter().find(|c| c.id == id),
        }
    }

    fn selected_card(&self) -> Option<&Card> {
        self.selected.as_deref().and_then(|id| self.card(id))
    }

    fn update_card(&mut self, id: &str, cx: &mut Context<Self>, f: impl FnOnce(&mut Card)) {
        let Some(ix) = self.card(id).and_then(|card| self.cards.iter().position(|c| std::ptr::eq(c, card)))
        else {
            return;
        };
        let row_key = |this: &Self, card: &Card| {
            (this.filter.accepts(card) && card.matches(&this.query), card.pinned)
        };
        let before = (row_key(self, &self.cards[ix]), stats_key(&self.cards[ix], &self.queries));
        f(&mut self.cards[ix]);
        self.cards[ix].touch();
        let after = (row_key(self, &self.cards[ix]), stats_key(&self.cards[ix], &self.queries));

        self.persist(id, None, cx);
        if before.1 != after.1 {
            self.stats = Stats::compute(&self.cards, &self.queries);
        }
        if before.0 != after.0 {
            // The card entered, left or moved within the list.
            self.refresh_rows(false, Some(id));
        } else if let Some(row) = self.rows.iter().position(|r| matches!(r, Row::Card(c) if c == id)) {
            // Same place, maybe a different height.
            self.list_state.remeasure_items(row..row + 1);
        }
        cx.notify();
    }

    fn update_selected(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Card)) {
        if let Some(id) = self.selected.clone() {
            self.update_card(&id, cx, f);
            self.keep_selected(cx);
        }
    }

    fn visible_cards(&self) -> Vec<&Card> {
        self.cards
            .iter()
            .filter(|c| self.filter.accepts(c) && c.matches(&self.query))
            .collect()
    }

    /// The rows for the current filter and search: pinned first, then by day.
    fn compute_rows(&self) -> Vec<Row> {
        let cards = self.visible_cards();
        let (pinned, rest): (Vec<&Card>, Vec<&Card>) = cards.into_iter().partition(|c| c.pinned);
        let mut rows = Vec::with_capacity(pinned.len() + rest.len() + 64);
        if !pinned.is_empty() {
            rows.push(Row::Section {
                day: None,
                count: pinned.len(),
            });
            rows.extend(pinned.iter().map(|c| Row::Card(c.id.clone())));
        }
        // `self.cards` is sorted newest first, so days come out in order.
        let mut section_ix = None;
        let mut current_day = None;
        for card in rest {
            let day = card.created.with_timezone(&Local).date_naive();
            if current_day != Some(day) {
                current_day = Some(day);
                section_ix = Some(rows.len());
                rows.push(Row::Section {
                    day: Some(day),
                    count: 0,
                });
            }
            if let Some(Row::Section { count, .. }) = section_ix.and_then(|ix| rows.get_mut(ix)) {
                *count += 1;
            }
            rows.push(Row::Card(card.id.clone()));
        }
        rows
    }

    /// Recompute the rows and tell the list what changed.
    ///
    /// `reset` scrolls back to the top (new filter or search); otherwise only
    /// the changed range is replaced so the scroll position stays put.
    /// `changed` is a card whose content (and so height) may have changed.
    fn refresh_rows(&mut self, reset: bool, changed: Option<&str>) {
        self.index = self
            .cards
            .iter()
            .enumerate()
            .map(|(ix, c)| (c.id.clone(), ix))
            .collect();
        self.stats = Stats::compute(&self.cards, &self.queries);
        let rows = crate::profile::time("compute_rows", || self.compute_rows());
        self.visible_count = rows.iter().filter(|r| matches!(r, Row::Card(_))).count();
        if reset {
            self.list_state.reset(rows.len());
        } else {
            let old = &self.rows;
            let prefix = old.iter().zip(&rows).take_while(|(a, b)| a == b).count();
            let max_suffix = old.len().min(rows.len()) - prefix;
            let suffix = old
                .iter()
                .rev()
                .zip(rows.iter().rev())
                .take(max_suffix)
                .take_while(|(a, b)| a == b)
                .count();
            if prefix != old.len() || old.len() != rows.len() {
                self.list_state
                    .splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
            }
            if let Some(id) = changed
                && let Some(ix) = rows.iter().position(|r| matches!(r, Row::Card(c) if c == id))
            {
                self.list_state.remeasure_items(ix..ix + 1);
            }
        }
        self.rows = rows;
    }

    // ----- body editor ------------------------------------------------------

    fn set_body_language(&mut self, language: &str, cx: &mut Context<Self>) {
        if self.body_language.as_ref() != language {
            self.body_language = SharedString::from(language.to_string());
            let language = self.body_language.clone();
            self.body.update(cx, |s, cx| s.set_highlighter(language, cx));
        }
    }

    /// Configure the shared body editor for the selected card's kind.
    fn configure_body(&mut self, card: &Card, window: &mut Window, cx: &mut Context<Self>) {
        let snippet = card.kind == CardKind::Snippet;
        let language = if snippet { card.language().to_string() } else { "markdown".into() };
        self.set_body_language(&language, cx);
        let body = card.body.clone();
        self.body.update(cx, |s, cx| {
            s.set_line_number(snippet, window, cx);
            s.set_folding(snippet, window, cx);
            s.set_soft_wrap(!snippet, window, cx);
            s.set_placeholder(
                match card.kind {
                    CardKind::Note => "Start writing… Markdown is supported",
                    CardKind::Todo => "Add a description…",
                    CardKind::Snippet => "Paste or write code…",
                },
                window,
                cx,
            );
            s.set_value(body, window, cx);
        });
    }

    // ----- actions ----------------------------------------------------------

    fn create(&mut self, kind: CardKind, body: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.vault.is_none() {
            return;
        }
        let mut card = Card::new(kind);
        if let Some(body) = body {
            card.body = body;
        }
        match &self.filter {
            Filter::Tag(tag) => card.tags.push(tag.clone()),
            Filter::Project(project) => card.project = Some(project.clone()),
            Filter::Component(project, component) => {
                card.project = Some(project.clone());
                card.component = Some(component.clone());
            }
            Filter::Feature(project, component, feature) => {
                card.project = Some(project.clone());
                card.component = Some(component.clone());
                card.feature = Some(feature.clone());
            }
            _ => {}
        }
        // Make sure the new card is visible.
        let reset = !self.filter.accepts(&card);
        if reset {
            self.filter = Filter::All;
        }
        let id = card.id.clone();
        self.cards.insert(0, card);
        self.refresh_rows(reset, None);
        self.persist(&id, Some(window), cx);
        self.open(&id, window, cx);
        self.preview = false;

        match kind {
            CardKind::Todo => self.item_input.update(cx, |s, cx| s.focus(window, cx)),
            _ => self.body.update(cx, |s, cx| s.focus(window, cx)),
        }
    }

    fn open(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(id) {
            return;
        }
        // Kept cards stay in the sidebar; a preview is replaced.
        self.hide(window, cx);
        let Some(card) = self.card(id).cloned() else {
            return;
        };
        self.selected = Some(card.id.clone());
        self.opened_at = Some(Instant::now());
        self.preview = card.kind == CardKind::Note && !card.body.trim().is_empty();
        self.title
            .update(cx, |s, cx| s.set_value(card.title.clone(), window, cx));
        self.configure_body(&card, window, cx);
        self.tag_input.update(cx, |s, cx| s.set_value("", window, cx));
        self.item_input.update(cx, |s, cx| s.set_value("", window, cx));
        self.sync_project_inputs(window, cx);
        cx.notify();
    }

    /// Hide the popup. Kept cards stay in the sidebar, a preview is gone.
    fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        self.discard_if_empty(window, cx);
        self.selected = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn is_kept(&self, id: &str) -> bool {
        self.ui.minimized.iter().any(|m| m == id)
    }

    /// Turn the open preview into a card kept in the sidebar.
    fn keep_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone()
            && !self.is_kept(&id)
        {
            self.ui.minimized.push(id);
            self.save_settings();
            cx.notify();
        }
    }

    /// Keep the open card in the sidebar and hide the popup.
    fn minimize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_selected(cx);
        self.hide(window, cx);
    }

    /// Remove a card from the sidebar, closing it if it is open.
    fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_kept(id) {
            self.ui.minimized.retain(|m| m != id);
            self.save_settings();
        }
        if self.selected.as_deref() == Some(id) {
            self.hide(window, cx);
        }
        cx.notify();
    }

    /// The second click of a double click on a card that just opened.
    fn is_double_click_on_open(&self, event: &MouseDownEvent) -> bool {
        event.button == MouseButton::Left
            && event.click_count >= 2
            && self.opened_at.is_some_and(|t| t.elapsed() < DOUBLE_CLICK_WINDOW)
    }

    fn save_settings(&mut self) {
        self.config.sidebar_width = self.ui.sidebar_width;
        self.config.dock_width = self.ui.dock_width;
        if let Err(err) = self.config.save() {
            eprintln!("Saving settings failed: {err}");
        }
        if let Some(vault) = &self.vault {
            let state = VaultState {
                minimized: self.ui.minimized.clone(),
                queries: self.queries.clone(),
                expanded_projects: self.expanded.iter().cloned().collect(),
            };
            if let Err(err) = vault.save_state(&state) {
                eprintln!("Saving vault state failed: {err}");
            }
        }
    }

    /// Cards left empty when closing the editor are dropped silently.
    fn discard_if_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.take() {
            let before = self.cards.len();
            self.cards.retain(|c| c.id != id || !c.is_empty());
            if self.cards.len() != before {
                self.remove_file(&id, Some(window), cx);
                self.refresh_rows(false, None);
                if self.is_kept(&id) {
                    self.ui.minimized.retain(|m| *m != id);
                    self.save_settings();
                }
            }
        }
    }

    fn delete(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.cards.iter().position(|c| c.id == id) else {
            return;
        };
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
            self.focus_handle.focus(window, cx);
        }
        let card = self.cards.remove(index);
        self.remove_file(&card.id, Some(window), cx);
        self.refresh_rows(false, None);
        if self.ui.minimized.iter().any(|m| *m == card.id) {
            self.ui.minimized.retain(|m| *m != card.id);
            self.save_settings();
        }
        if !card.is_empty() {
            let dismiss = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(6)).await;
                let _ = this.update(cx, |this, cx| {
                    this.undo = None;
                    cx.notify();
                });
            });
            self.undo = Some(Undo {
                index,
                card,
                _dismiss: dismiss,
            });
        }
        cx.notify();
    }

    fn undo_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(undo) = self.undo.take() {
            let index = undo.index.min(self.cards.len());
            let id = undo.card.id.clone();
            self.cards.insert(index, undo.card);
            self.persist(&id, Some(window), cx);
            self.refresh_rows(false, None);
            cx.notify();
        }
    }

    fn set_kind(&mut self, kind: CardKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if let Some(card) = self.cards.iter_mut().find(|c| c.id == id) {
            card.convert(kind);
            let card = card.clone();
            self.preview = false;
            self.configure_body(&card, window, cx);
            self.persist(&id, Some(window), cx);
            self.refresh_rows(false, Some(&id));
            cx.notify();
        }
    }

    fn set_language(&mut self, language: Option<String>, cx: &mut Context<Self>) {
        self.update_selected(cx, |card| card.language = language);
        if let Some(lang) = self.selected_card().map(|c| c.language().to_string()) {
            self.set_body_language(&lang, cx);
        }
    }

    fn set_preview(&mut self, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = preview;
        if !preview {
            self.body.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    /// Enter in a markdown list starts the next item, or ends the list on an
    /// empty one; Shift+Enter is a plain newline.
    ///
    /// Runs in the capture phase, before the editor's own newline: anything it
    /// handles must stop propagation.
    fn on_body_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if action.shift || action.secondary || self.selected_card().is_none_or(|c| c.kind == CardKind::Snippet) {
            return;
        }
        let edit = {
            let state = self.body.read(cx);
            let range = state.selected_range();
            if !range.is_empty() {
                return;
            }
            list_enter(&state.value(), range.end)
        };
        let Some(edit) = edit else {
            return;
        };
        cx.stop_propagation();
        self.body.update(cx, |s, cx| {
            s.set_selected_range(edit.range, cx);
            s.replace(edit.text, window, cx);
        });
    }

    fn paste_snippet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|item| item.text());
        match text.filter(|t| !t.trim().is_empty()) {
            Some(text) => {
                self.create(CardKind::Snippet, Some(text), window, cx);
                window.push_notification(Notification::success("Snippet pasted from clipboard"), cx);
            }
            None => window.push_notification(Notification::info("Clipboard has no text"), cx),
        }
    }

    fn copy_card(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(card) = self.card(id) {
            let text = match card.kind {
                CardKind::Todo => card.todo_markdown(),
                _ => card.body.clone(),
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            window.push_notification(Notification::success("Copied to clipboard"), cx);
        }
    }

    fn set_filter(&mut self, filter: Filter, window: &mut Window, cx: &mut Context<Self>) {
        self.hide(window, cx);
        // Picking a project or component shows what is inside it.
        let key = match &filter {
            Filter::Project(project) => Some(project.clone()),
            Filter::Component(project, component) => Some(component_key(project, component)),
            _ => None,
        };
        if let Some(key) = key
            && self.expanded.insert(key)
        {
            self.save_settings();
        }
        self.filter = filter;
        self.refresh_rows(true, None);
        cx.notify();
    }

    fn is_collapsed(&self, section: &str) -> bool {
        self.config.collapsed_sections.iter().any(|s| s == section)
    }

    fn toggle_section(&mut self, section: &str, cx: &mut Context<Self>) {
        if self.is_collapsed(section) {
            self.config.collapsed_sections.retain(|s| s != section);
        } else {
            self.config.collapsed_sections.push(section.to_string());
        }
        self.save_settings();
        cx.notify();
    }

    fn set_theme(&mut self, theme: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        self.config.theme = theme;
        self.save_settings();
        apply_theme(theme, window, cx);
    }

    // ----- attachments ------------------------------------------------------

    /// Whether files can be embedded in the open card; only notes take them.
    fn can_attach(&self) -> bool {
        self.selected_card().is_some_and(|c| c.kind == CardKind::Note)
    }

    /// Copy files into the vault; returns the Markdown that embeds them.
    fn import_files(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let vault = self.vault.as_ref()?;
        let mut embeds = Vec::new();
        let mut errors = Vec::new();
        for path in paths.iter().filter(|p| p.is_file()) {
            match vault.import_attachment(path) {
                Ok(link) => embeds.push(embed_markdown(&link)),
                Err(err) => errors.push(format!("{}: {err}", path.display())),
            }
        }
        if let Some(first) = errors.first() {
            self.report(format!("Attaching failed: {first}"), Some(window), cx);
        }
        // Paragraphs of their own, or images shrink to the height of a line.
        (!embeds.is_empty()).then(|| embeds.join("\n\n"))
    }

    /// Embed files in the open note.
    fn attach_files(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        if self.can_attach()
            && let Some(markdown) = self.import_files(paths, window, cx)
        {
            self.insert_embeds(markdown, window, cx);
        }
    }

    /// Add Markdown to the open note: at the cursor while writing, at the
    /// end in preview, where there is no cursor.
    fn insert_embeds(&mut self, markdown: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.selected_card().filter(|c| c.kind == CardKind::Note) else {
            return;
        };
        let body = if self.preview {
            let body = card.body.trim_end();
            let body = if body.is_empty() { markdown } else { format!("{body}\n\n{markdown}") };
            self.body.update(cx, |s, cx| s.set_value(body.clone(), window, cx));
            body
        } else {
            self.body.update(cx, |s, cx| {
                // Embeds go in a paragraph of their own: blank lines around them.
                let value = s.value();
                let cursor = s.cursor();
                let before = value.get(..cursor).unwrap_or_default().trim_end_matches(' ');
                let after = value.get(cursor..).unwrap_or_default();
                let lead = match before {
                    "" => "",
                    b if b.ends_with("\n\n") => "",
                    b if b.ends_with('\n') => "\n",
                    _ => "\n\n",
                };
                let trail = if after.starts_with("\n\n") { "" } else if after.starts_with('\n') { "\n" } else { "\n\n" };
                s.insert(format!("{lead}{markdown}{trail}"), window, cx);
                s.focus(window, cx);
            });
            self.body.read(cx).value().to_string()
        };
        self.update_selected(cx, |card| card.body = body);
    }

    /// Choose files to embed in the open note.
    fn prompt_attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update_in(cx, |this, window, cx| this.attach_files(&paths, window, cx));
            }
        })
        .detach();
    }

    /// Files dropped on the card list go into the open note, or a new one.
    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_attach() {
            self.attach_files(paths.paths(), window, cx);
        } else if let Some(markdown) = self.import_files(paths.paths(), window, cx) {
            self.create(CardKind::Note, Some(markdown), window, cx);
            self.preview = true;
        }
    }

    /// Pasting files or an image into a note attaches them; text pastes as usual.
    ///
    /// Runs in the capture phase, before the editor's own paste: anything it
    /// handles must stop propagation.
    fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard().filter(|_| self.can_attach()) else {
            return;
        };
        let mut paths = Vec::new();
        let mut image = None;
        let mut text = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(p) => paths.extend(p.paths().iter().cloned()),
                ClipboardEntry::Image(img) => image = image.or(Some(img)),
                ClipboardEntry::String(_) => text = true,
            }
        }
        if !paths.is_empty() {
            // Copied files also come with their paths as text; skip that.
            cx.stop_propagation();
            self.attach_files(&paths, window, cx);
            return;
        }
        // Apps often put a picture of copied text next to it; text wins.
        let (Some(image), false) = (image, text) else {
            return;
        };
        cx.stop_propagation();
        let (ext, bytes) = match image.format {
            ImageFormat::Png => ("png", image.bytes.clone()),
            ImageFormat::Jpeg => ("jpg", image.bytes.clone()),
            ImageFormat::Webp => ("webp", image.bytes.clone()),
            ImageFormat::Gif => ("gif", image.bytes.clone()),
            ImageFormat::Svg => ("svg", image.bytes.clone()),
            // Windows hands out screenshots as uncompressed bitmaps.
            ImageFormat::Bmp | ImageFormat::Tiff | ImageFormat::Ico | ImageFormat::Pnm => {
                match to_png(&image.bytes) {
                    Ok(png) => ("png", png),
                    Err(err) => {
                        self.report(format!("Reading the image failed: {err}"), Some(window), cx);
                        return;
                    }
                }
            }
        };
        let name = format!("pasted-{}.{ext}", Local::now().format("%Y%m%d-%H%M%S"));
        let Some(vault) = &self.vault else {
            return;
        };
        match vault.save_attachment(&name, &bytes) {
            Ok(link) => self.insert_embeds(embed_markdown(&link), window, cx),
            Err(err) => self.report(format!("Saving the image failed: {err}"), Some(window), cx),
        }
    }

    // ----- projects ---------------------------------------------------------

    fn set_project(&mut self, project: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // Blur also commits, so ignore it when nothing changed.
        if self.selected_card().is_some_and(|c| c.project != project) {
            self.update_selected(cx, |card| card.set_project(project));
        }
        self.sync_project_inputs(window, cx);
    }

    fn set_component(&mut self, component: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.selected_card() else {
            return;
        };
        let component = component.filter(|_| card.project.is_some());
        if card.component != component {
            self.update_selected(cx, |card| card.set_component(component));
        }
        self.sync_project_inputs(window, cx);
    }

    fn set_feature(&mut self, feature: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.selected_card() else {
            return;
        };
        let feature = feature.filter(|_| card.component.is_some());
        if card.feature != feature {
            self.update_selected(cx, |card| card.set_feature(feature));
        }
        self.sync_project_inputs(window, cx);
    }

    /// Show the open card's project, component and feature in the editor inputs.
    fn sync_project_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let card = self.selected_card();
        let value = |f: fn(&Card) -> &Option<String>| card.and_then(|c| f(c).clone()).unwrap_or_default();
        let fields = [
            (&self.project_input, value(|c| &c.project)),
            (&self.component_input, value(|c| &c.component)),
            (&self.feature_input, value(|c| &c.feature)),
        ];
        for (input, value) in fields {
            input.update(cx, |s, cx| {
                if s.value() != value.as_str() {
                    s.set_value(value, window, cx);
                }
            });
        }
    }

    /// Show or hide the children of a project or `project/component`.
    fn toggle_expanded(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_string());
        }
        self.save_settings();
        cx.notify();
    }

    // ----- saved queries ----------------------------------------------------

    /// The current filter and search as one query.
    fn current_query(&self) -> String {
        let filter = self.filter.as_query();
        let search = self.query.split_whitespace().collect::<Vec<_>>().join(" ");
        [filter, search]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Open the saved query form: a new query starts from the current view.
    fn open_query_form(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let (name, query) = match index.and_then(|ix| self.queries.get(ix)) {
            Some(saved) => (saved.name.clone(), saved.query.clone()),
            None if index.is_some() => return,
            None => (String::new(), self.current_query()),
        };
        // One popup at a time.
        self.hide(window, cx);
        self.query_form = Some(QueryForm { index });
        self.query_text.update(cx, |s, cx| s.set_value(query, window, cx));
        self.query_name.update(cx, |s, cx| {
            s.set_value(name, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// Fill the form's query with the current filter and search.
    fn use_current_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.current_query();
        self.query_text.update(cx, |s, cx| s.set_value(query, window, cx));
        cx.notify();
    }

    fn form_query(&self, cx: &App) -> String {
        let text = self.query_text.read(cx).value().to_string();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn submit_query_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.query_form else {
            return;
        };
        let query = self.form_query(cx);
        if query.is_empty() {
            self.query_text.update(cx, |s, cx| s.focus(window, cx));
            return;
        }
        let name = self.query_name.read(cx).value().trim().to_string();
        let saved = SavedQuery {
            name: if name.is_empty() { query.clone() } else { name },
            query,
        };
        match form.index {
            Some(ix) if ix < self.queries.len() => self.queries[ix] = saved.clone(),
            _ => {
                self.queries.push(saved.clone());
                // A new query usually holds the search it was made from.
                self.query.clear();
                self.search.update(cx, |s, cx| s.set_value("", window, cx));
            }
        }
        self.query_form = None;
        self.save_settings();
        self.focus_handle.focus(window, cx);
        self.set_filter(Filter::Saved(saved), window, cx);
    }

    fn close_query_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query_form.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn delete_query(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.queries.len() {
            return;
        }
        let saved = self.queries.remove(ix);
        // The form's index would now point at another query.
        self.query_form = None;
        self.save_settings();
        if self.filter == Filter::Saved(saved) {
            self.set_filter(Filter::All, window, cx);
        }
        cx.notify();
    }

    // ----- vaults -----------------------------------------------------------

    /// Read a vault on a background thread, then switch to it.
    fn open_vault(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.spawn_in(window, {
            let path = path.clone();
            async move |this, cx| {
                let read_path = path.clone();
                let loaded = cx
                    .background_executor()
                    .spawn(async move { crate::profile::time("load_vault", || Vault::open(&read_path)) })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.loading = None;
                    this.finish_open_vault(path, loaded, window, cx);
                });
            }
        });
        // Replacing an earlier load drops (cancels) it.
        self.loading = Some((path, task));
        cx.notify();
    }

    fn finish_open_vault(
        &mut self,
        path: PathBuf,
        loaded: std::io::Result<LoadedVault>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let LoadedVault {
            mut vault,
            mut cards,
            errors,
        } = match loaded {
            Ok(loaded) => loaded,
            Err(err) => {
                window.push_notification(
                    Notification::error(format!("Could not open vault {}: {err}", path.display())),
                    cx,
                );
                return;
            }
        };

        // Bring cards from the pre-vault cards.json into the first empty vault.
        let mut migrated = 0;
        if cards.is_empty()
            && let Some(legacy) = load_legacy_cards()
        {
            for card in &legacy {
                match vault.save_card(card) {
                    Ok(()) => migrated += 1,
                    Err(err) => eprintln!("Importing card failed: {err}"),
                }
            }
            if migrated == legacy.len() {
                let _ = retire_legacy_cards();
            }
            cards = legacy;
        }

        self.hide(window, cx);
        self.undo = None;
        self.filter = Filter::All;
        self.query.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.query_form = None;
        self.cards = cards;
        self.sort();
        let state = vault.load_state();
        self.queries = state.queries;
        self.expanded = state.expanded_projects.into_iter().collect();
        self.refresh_rows(true, None);
        self.ui.minimized = state
            .minimized
            .into_iter()
            .filter(|id| self.cards.iter().any(|c| c.id == *id))
            .collect();
        window.set_window_title(&format!("{} — Zettelkasten", vault.name()));
        self.vault = Some(vault);
        self.config.remember(&path);
        self.save_settings();

        if migrated > 0 {
            window.push_notification(
                Notification::success(format!("Imported {migrated} cards from the previous version")),
                cx,
            );
        }
        if let Some(first) = errors.first() {
            window.push_notification(
                Notification::error(format!(
                    "{} file(s) could not be read, e.g. {first}",
                    errors.len()
                )),
                cx,
            );
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn close_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide(window, cx);
        self.save_settings();
        self.vault = None;
        self.cards.clear();
        self.queries.clear();
        self.expanded.clear();
        self.query_form = None;
        self.filter = Filter::All;
        self.refresh_rows(true, None);
        self.ui.minimized.clear();
        self.undo = None;
        self.config.last_vault = None;
        self.save_settings();
        window.set_window_title("Zettelkasten");
        cx.notify();
    }

    fn forget_vault(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.config.forget(&path);
        self.save_settings();
        cx.notify();
    }

    /// Pick any folder and use it as a vault.
    fn prompt_open_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open vault".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| this.open_vault(path, window, cx));
            }
        })
        .detach();
    }

    /// Choose a name and location for a new vault folder.
    fn prompt_new_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let path = cx.prompt_for_new_path(&start, Some("Zettelkasten"));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = path.await {
                let _ = this.update_in(cx, |this, window, cx| this.open_vault(path, window, cx));
            }
        })
        .detach();
    }

    fn on_open_vault(&mut self, _: &OpenVault, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_open_vault(window, cx);
    }

    // ----- resizing ---------------------------------------------------------

    fn on_resize_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(resizing) = self.resizing else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.finish_resize(cx);
            return;
        }
        let viewport = f32::from(window.viewport_size().width);
        let x = f32::from(event.position.x);
        match resizing {
            Resizing::Sidebar => {
                let max = SIDEBAR_RANGE.1.min(viewport - MAIN_MIN);
                self.ui.sidebar_width = x.clamp(SIDEBAR_RANGE.0, max.max(SIDEBAR_RANGE.0));
            }
            Resizing::Dock => {
                let max = DOCK_RANGE.1.min(viewport - self.ui.sidebar_width - MAIN_MIN);
                self.ui.dock_width = (viewport - x).clamp(DOCK_RANGE.0, max.max(DOCK_RANGE.0));
            }
        }
        cx.notify();
    }

    fn finish_resize(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            self.save_settings();
            cx.notify();
        }
    }

    // ----- action handlers --------------------------------------------------

    fn on_new_note(&mut self, _: &NewNote, window: &mut Window, cx: &mut Context<Self>) {
        self.create(CardKind::Note, None, window, cx);
    }

    fn on_new_todo(&mut self, _: &NewTodo, window: &mut Window, cx: &mut Context<Self>) {
        self.create(CardKind::Todo, None, window, cx);
    }

    fn on_new_snippet(&mut self, _: &NewSnippet, window: &mut Window, cx: &mut Context<Self>) {
        self.create(CardKind::Snippet, None, window, cx);
    }

    fn on_paste_snippet(&mut self, _: &PasteSnippet, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_snippet(window, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn on_close_editor(&mut self, _: &CloseEditor, window: &mut Window, cx: &mut Context<Self>) {
        if self.query_form.is_some() {
            self.close_query_form(window, cx);
            return;
        }
        self.hide(window, cx);
    }

    fn on_delete_card(&mut self, _: &DeleteCard, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone() {
            self.delete(&id, window, cx);
        }
    }

    fn on_toggle_pin(&mut self, _: &TogglePin, _: &mut Window, cx: &mut Context<Self>) {
        self.update_selected(cx, |card| card.pinned = !card.pinned);
    }

    fn on_minimize(&mut self, _: &MinimizeCard, window: &mut Window, cx: &mut Context<Self>) {
        self.minimize(window, cx);
    }

    fn on_toggle_preview(&mut self, _: &TogglePreview, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_card().is_some_and(|c| c.kind == CardKind::Note) {
            self.set_preview(!self.preview, window, cx);
        }
    }
}

// ----- rendering ------------------------------------------------------------

fn kind_icon(kind: CardKind) -> IconName {
    match kind {
        CardKind::Note => IconName::StickyNote,
        CardKind::Todo => IconName::ListTodo,
        CardKind::Snippet => IconName::Code,
    }
}

fn accent(color: CardColor) -> Hsla {
    rgb(color.hex()).into()
}

fn modal_radius(theme: &gpui_kit::component::Theme) -> Pixels {
    theme.radius_lg * 1.5
}

fn kbd(keys: &str) -> Kbd {
    Kbd::new(Keystroke::parse(keys).expect("valid keystroke"))
}

/// Markdown style for compact card previews.
fn card_markdown_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, base| match level {
            1 => base * 1.25,
            2 => base * 1.15,
            _ => base * 1.05,
        })
}

/// Switch to the light or dark theme the preference asks for.
fn apply_theme(theme: ThemePreference, window: &mut Window, cx: &mut App) {
    match theme {
        ThemePreference::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        ThemePreference::System => Theme::sync_system_appearance(Some(window), cx),
    }
}

fn file_icon(name: &str) -> IconName {
    let name = name.to_lowercase();
    if is_image(&name) {
        IconName::Image
    } else if [".pdf", ".txt", ".md", ".doc", ".docx", ".rtf"].iter().any(|ext| name.ends_with(ext)) {
        IconName::FileText
    } else {
        IconName::File
    }
}

fn to_png(bytes: &[u8]) -> image::ImageResult<Vec<u8>> {
    let mut png = Vec::new();
    image::load_from_memory(bytes)?
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// Where an image in a card loads from: a file in the vault or a web URL.
fn image_source(root: &Path, target: &str) -> Option<ImageSource> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Some(SharedUri::from(target.to_string()).into());
    }
    link_path(root, target).map(ImageSource::from)
}

/// Open a link in a card: files with their app, web links in the browser.
fn open_link(root: &Path, link: &str, window: &mut Window, cx: &mut App) {
    match link_path(root, link) {
        Some(path) if path.exists() => cx.open_with_system(&path),
        Some(path) => window.push_notification(
            Notification::error(format!("{} not found", path.display())),
            cx,
        ),
        None if link.starts_with('#') => {}
        None => cx.open_url(link),
    }
}

/// Markdown whose images and file links resolve inside the vault.
fn vault_markdown(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    root: &Path,
) -> gpui_kit::base::TextView {
    let images = root.to_path_buf();
    let links = root.to_path_buf();
    gpui_kit::base::TextView::markdown(id, text)
        .image_source(move |uri| {
            let uri = uri.to_string();
            image_source(&images, &uri).unwrap_or_else(|| SharedUri::from(uri).into())
        })
        .on_link_click(move |url, _, window, cx| open_link(&links, url, window, cx))
}

impl ZettelApp {
    /// Image previews for a card in the list.
    fn render_thumbnails(&self, card: &Card, cx: &mut Context<Self>) -> Option<AnyElement> {
        const SHOWN: usize = 3;
        let root = self.vault.as_ref()?.root();
        let sources: Vec<ImageSource> = md_links(&card.body)
            .iter()
            .filter(|l| l.image)
            .filter_map(|l| image_source(root, &l.target))
            .collect();
        if sources.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let more = sources.len().saturating_sub(SHOWN);
        // A single image is shown whole, several are cropped into tiles.
        let single = sources.len() == 1;
        let height = px(if single { 200. } else { 120. });
        let muted = theme.muted_foreground;
        Some(
            h_flex()
                .gap_2()
                .children(sources.into_iter().take(SHOWN).enumerate().map(|(ix, source)| {
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h(height)
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .overflow_hidden()
                        .child(
                            // Out of the flow, so the image's own size can't stretch the box.
                            img(source)
                                .absolute()
                                .inset_0()
                                .size_full()
                                .rounded(theme.radius)
                                .object_fit(if single { ObjectFit::Contain } else { ObjectFit::Cover })
                                .with_fallback(move || {
                                    div()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(Icon::new(IconName::ImageOff).text_color(muted))
                                        .into_any_element()
                                }),
                        )
                        .when(more > 0 && ix == SHOWN - 1, |s| {
                            s.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(theme.radius)
                                    .bg(gpui_kit::black().opacity(0.45))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(gpui_kit::white())
                                    .font_semibold()
                                    .child(format!("+{more}")),
                            )
                        })
                }))
                .into_any_element(),
        )
    }

    /// Chips for the files a note links to; a click opens the file.
    /// `detailed` adds the file size, which reads the disk.
    fn render_file_chips(&self, card: &Card, detailed: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let root = self.vault.as_ref()?.root().to_path_buf();
        let mut files: Vec<MdLink> = md_links(&card.body)
            .into_iter()
            .filter(|l| !l.image && l.is_local())
            .collect();
        files.dedup_by(|a, b| a.target == b.target);
        if files.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        Some(
            h_flex()
                .gap_1p5()
                .flex_wrap()
                .children(files.into_iter().enumerate().map(|(ix, link)| {
                    let size = detailed
                        .then(|| link_path(&root, &link.target))
                        .flatten()
                        .map(|path| std::fs::metadata(path).map(|m| human_size(m.len())));
                    let missing = matches!(size, Some(Err(_)));
                    let root = root.clone();
                    let target = link.target.clone();
                    h_flex()
                        .id(SharedString::from(format!("file-{}-{ix}", card.id)))
                        .gap_1p5()
                        .max_w(px(280.))
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background.opacity(0.6))
                        .text_xs()
                        .cursor_pointer()
                        .hover(|s| s.border_color(theme.ring).bg(theme.secondary))
                        .when(missing, |s| s.opacity(0.55))
                        .child(Icon::new(file_icon(&link.target)).xsmall().text_color(theme.muted_foreground))
                        .child(div().min_w_0().truncate().child(link.file_name().to_string()))
                        .children(size.map(|size| {
                            div()
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(size.unwrap_or_else(|_| "missing".into()))
                        }))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Open").build(window, cx)
                        })
                        // Opening the file should not open the card too.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            open_link(&root, &target, window, cx);
                        })
                }))
                .into_any_element(),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let count = |f: &Filter| self.stats.count(f);

        // `leading` goes before the icon: a project's chevron or a component's indent.
        let nav_item = |id: &str,
                        icon: IconName,
                        label: String,
                        filter: Filter,
                        highlight: Option<Hsla>,
                        leading: Option<AnyElement>,
                        cx: &mut Context<Self>| {
            let theme = cx.theme().clone();
            let active = self.filter == filter;
            let n = count(&filter);
            h_flex()
                .id(SharedString::from(format!("nav-{id}-{label}")))
                .gap_2p5()
                .px_2p5()
                .py_1p5()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .when(active, |s| s.bg(theme.sidebar_accent).font_medium())
                .hover(|s| s.bg(theme.sidebar_accent).text_color(theme.foreground))
                .children(leading)
                .child(Icon::new(icon).small())
                .child(div().flex_1().truncate().child(label))
                .child(match highlight.filter(|_| n > 0) {
                    Some(color) => div()
                        .px_1p5()
                        .min_w(px(20.))
                        .flex()
                        .justify_center()
                        .rounded_full()
                        .text_xs()
                        .font_semibold()
                        .bg(color.opacity(0.18))
                        .text_color(color)
                        .child(n.to_string()),
                    None => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(n.to_string()),
                })
                .on_click(cx.listener(move |this, _, window, cx| this.set_filter(filter.clone(), window, cx)))
        };

        let section = |label: &'static str| {
            div()
                .px_2p5()
                .pt_4()
                .pb_1p5()
                .text_xs()
                .font_medium()
                .text_color(theme.muted_foreground.opacity(0.8))
                .child(label)
        };

        // A section header that collapses its list; `extra` sits next to the chevron.
        let collapsible = |label: &'static str, extra: Option<AnyElement>, cx: &mut Context<Self>| {
            let collapsed = self.is_collapsed(label);
            h_flex()
                .id(SharedString::from(format!("section-{label}")))
                .gap_1()
                .px_2p5()
                .pt_4()
                .pb_1p5()
                .text_xs()
                .font_medium()
                .cursor_pointer()
                .text_color(theme.muted_foreground.opacity(0.8))
                .hover(|s| s.text_color(theme.foreground))
                .child(div().flex_1().child(label))
                .children(extra)
                .child(
                    Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown })
                        .xsmall(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(label, cx)))
        };

        let mut tag_list = v_flex().gap_0p5();
        for tag in self.stats.tags.keys() {
            tag_list = tag_list.child(nav_item(
                "tag",
                IconName::Hash,
                tag.clone(),
                Filter::Tag(tag.clone()),
                None,
                None,
                cx,
            ));
        }

        let projects = self.render_projects(&nav_item, cx);
        let saved = self.render_saved_queries(&nav_item, cx);
        let add_query = div()
            .id("add-query")
            .size_4()
            .rounded(px(3.))
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(theme.foreground.opacity(0.1)))
            .child(Icon::new(IconName::Plus).xsmall())
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Save the current filter and search")
                    .build(window, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.open_query_form(None, window, cx);
            }));
        let saved_header = collapsible(SAVED_SECTION, Some(add_query.into_any_element()), cx);
        let projects_header = collapsible(PROJECTS_SECTION, None, cx);
        let tags_header = collapsible(TAGS_SECTION, None, cx);

        let vault_root = self.vault.as_ref().map(|v| v.root().to_path_buf()).unwrap_or_default();
        let path = vault_root.clone();
        let mut library = v_flex()
            .gap_0p5()
            .child(nav_item("all", IconName::Layers, "All cards".into(), Filter::All, None, None, cx))
            .child(nav_item("pinned", IconName::Pin, "Pinned".into(), Filter::Pinned, None, None, cx));
        for kind in CardKind::ALL {
            library = library.child(nav_item(
                "kind",
                kind_icon(kind),
                kind.plural().into(),
                Filter::Kind(kind),
                None,
                None,
                cx,
            ));
        }
        let open = nav_item(
            "open",
            IconName::Square,
            "Open todos".into(),
            Filter::OpenTodos,
            Some(accent(CardColor::Yellow)),
            None,
            cx,
        );

        v_flex()
            .w(px(self.ui.sidebar_width))
            .flex_none()
            .h_full()
            .bg(theme.sidebar)
            .child(
                v_flex()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .overflow_y_scrollbar()
                    .child(section("LIBRARY"))
                    .child(library)
                    .child(section("FOCUS"))
                    .child(open)
                    .child(saved_header)
                    .when(!self.is_collapsed(SAVED_SECTION), |s| s.child(saved))
                    .child(projects_header)
                    .when(!self.is_collapsed(PROJECTS_SECTION), |s| s.child(projects))
                    .child(tags_header)
                    .when(!self.is_collapsed(TAGS_SECTION), |s| s.child(tag_list)),
            )
            .child(
                h_flex()
                    .id("storage")
                    .gap_2()
                    .mx_3()
                    .mb_3()
                    .px_2p5()
                    .py_2()
                    .rounded(theme.radius)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.sidebar_accent))
                    .child(Icon::new(IconName::FolderOpen).xsmall())
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(format!("{} cards · {}", self.cards.len(), vault_name(&vault_root))),
                    )
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(path.display().to_string())
                            .build(window, cx)
                    })
                    .on_click(move |_, _, cx| cx.open_with_system(&vault_root)),
            )
            .into_any_element()
    }

    /// Projects, with components and their features nested below when expanded.
    fn render_projects(
        &self,
        nav_item: &impl Fn(&str, IconName, String, Filter, Option<Hsla>, Option<AnyElement>, &mut Context<Self>) -> Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        const INDENT: f32 = 14.;
        // Indent plus a chevron that expands `key`, or an empty slot of the same width.
        let leading = |depth: usize, key: Option<(String, bool)>, cx: &mut Context<Self>| {
            let slot = div()
                .id(SharedString::from(format!("toggle-{}", key.as_ref().map_or("", |k| k.0.as_str()))))
                .flex_none()
                .size_4()
                .rounded(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .when_some(key, |s, (key, expanded)| {
                    s.hover(|s| s.bg(theme.foreground.opacity(0.1)))
                        .child(
                            Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                                .xsmall(),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_expanded(&key, cx);
                            }),
                        )
                });
            h_flex()
                .flex_none()
                .pl(px(INDENT * depth as f32))
                .child(slot)
                .into_any_element()
        };

        let mut list = v_flex().gap_0p5();
        for (project, stats) in &self.stats.projects {
            let expanded = self.expanded.contains(project);
            let toggle = (!stats.components.is_empty()).then(|| (project.clone(), expanded));
            list = list.child(nav_item(
                "project",
                IconName::Folder,
                project.clone(),
                Filter::Project(project.clone()),
                None,
                Some(leading(0, toggle, cx)),
                cx,
            ));
            if !expanded {
                continue;
            }
            for (component, component_stats) in &stats.components {
                let key = component_key(project, component);
                let expanded = self.expanded.contains(&key);
                let toggle = (!component_stats.features.is_empty()).then(|| (key.clone(), expanded));
                list = list.child(nav_item(
                    &format!("component-{project}"),
                    IconName::Component,
                    component.clone(),
                    Filter::Component(project.clone(), component.clone()),
                    None,
                    Some(leading(1, toggle, cx)),
                    cx,
                ));
                if !expanded {
                    continue;
                }
                for feature in component_stats.features.keys() {
                    list = list.child(nav_item(
                        &format!("feature-{key}"),
                        IconName::Flag,
                        feature.clone(),
                        Filter::Feature(project.clone(), component.clone(), feature.clone()),
                        None,
                        Some(leading(2, None, cx)),
                        cx,
                    ));
                }
            }
        }
        list.into_any_element()
    }

    /// Saved queries, with an edit button on hover and a context menu.
    fn render_saved_queries(
        &self,
        nav_item: &impl Fn(&str, IconName, String, Filter, Option<Hsla>, Option<AnyElement>, &mut Context<Self>) -> Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let mut list = v_flex().gap_0p5();
        let this = cx.entity().downgrade();
        for (ix, saved) in self.queries.iter().enumerate() {
            let query = saved.query.clone();
            let group: SharedString = format!("saved-{ix}").into();
            let this = this.clone();
            // Covers the count on hover, so it takes no room otherwise.
            let edit = div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(6.))
                .flex()
                .items_center()
                .opacity(0.)
                .group_hover(group.clone(), |s| s.opacity(1.))
                .child(
                    div()
                        .id(SharedString::from(format!("edit-saved-{ix}")))
                        .size_5()
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.sidebar_accent)
                        .hover(|s| s.bg(theme.foreground.opacity(0.15)))
                        .child(Icon::new(IconName::Pencil).xsmall())
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Edit name and query")
                                .build(window, cx)
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_query_form(Some(ix), window, cx);
                            }),
                        ),
                );
            let item = nav_item(
                &format!("saved-{ix}"),
                IconName::Bookmark,
                saved.name.clone(),
                Filter::Saved(saved.clone()),
                None,
                None,
                cx,
            )
            .group(group)
            .relative()
            .child(edit)
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(query.clone()).build(window, cx)
            })
            .context_menu(move |menu, _, _| {
                let action = |f: fn(&mut Self, usize, &mut Window, &mut Context<Self>)| {
                    let this = this.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| f(this, ix, window, cx));
                    }
                };
                menu.item(PopupMenuItem::new("Edit…").icon(IconName::Pencil).on_click(action(
                    |this, ix, window, cx| this.open_query_form(Some(ix), window, cx),
                )))
                .separator()
                .item(
                    PopupMenuItem::new("Delete")
                        .icon(IconName::Trash)
                        .on_click(action(Self::delete_query)),
                )
            });
            list = list.child(item);
        }
        list.into_any_element()
    }

    /// Dialog for a saved query's display name and query, with a live match count.
    fn render_query_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let form = self.query_form?;
        let theme = cx.theme().clone();
        let query = self.form_query(cx);
        let matches = if query.is_empty() {
            "Type a query to see what it matches".to_string()
        } else {
            match self.cards.iter().filter(|c| c.matches(&query)).count() {
                0 => "No cards match".to_string(),
                1 => "1 card matches".to_string(),
                n => format!("{n} cards match"),
            }
        };
        let field = |label: &'static str, hint: &'static str, input: AnyElement| {
            v_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .justify_between()
                        .child(div().text_sm().font_medium().child(label))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(hint)),
                )
                .child(input)
        };
        let term = |syntax: &'static str, meaning: &'static str| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground.opacity(0.85))
                        .child(syntax),
                )
                .child(div().text_color(theme.muted_foreground).child(meaning))
        };
        let animation = || Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint());

        let dialog = v_flex()
            .id("query-dialog")
            .w(px(480.))
            .max_w(relative(0.9))
            .gap_4()
            .p_5()
            .rounded(modal_radius(&theme))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_2xl()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Bookmark).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().text_lg().font_semibold().child(match form.index {
                        Some(_) => "Edit saved query",
                        None => "New saved query",
                    }))
                    .child(
                        Button::new("close-query-dialog")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, window, cx| this.close_query_form(window, cx))),
                    ),
            )
            .child(field(
                "Display name",
                "Shown in the sidebar",
                Input::new(&self.query_name).into_any_element(),
            ))
            .child(field(
                "Query",
                "Which cards it shows",
                Input::new(&self.query_text)
                    .prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground))
                    .into_any_element(),
            ))
            .child(
                h_flex()
                    .justify_between()
                    .text_sm()
                    .child(div().text_color(theme.muted_foreground).child(matches))
                    .child(
                        Button::new("use-current-view")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Funnel)
                            .label("Use current view")
                            .tooltip("Fill in the current sidebar filter and search")
                            .on_click(cx.listener(|this, _, window, cx| this.use_current_view(window, cx))),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.muted.opacity(0.5))
                    .text_xs()
                    .child(term("words", "Text in title, body, tasks or tags"))
                    .child(term("#tag", "Tag starting with this"))
                    .child(term("tag:name", "Exactly this tag"))
                    .child(term("project:name", "Also component:, feature:"))
                    .child(term("project:none", "Cards without a project"))
                    .child(term("is:open", "Also note, todo, snippet, pinned, done"))
                    .child(term("lang:rust", "Snippets in a language")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when_some(form.index, |s, ix| {
                        s.child(
                            Button::new("delete-query")
                                .ghost()
                                .small()
                                .icon(IconName::Trash)
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, window, cx| this.delete_query(ix, window, cx))),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("cancel-query")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close_query_form(window, cx))),
                    )
                    .child(
                        Button::new("save-query")
                            .primary()
                            .small()
                            .label("Save")
                            .on_click(cx.listener(|this, _, window, cx| this.submit_query_form(window, cx))),
                    ),
            )
            .with_animation("query-dialog-in", animation(), |el, t| {
                el.opacity(t).mt(px(14. * (1. - t)))
            });

        Some(
            div()
                .id("query-dialog-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_kit::black().opacity(0.55))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_query_form(window, cx)),
                )
                .child(dialog)
                .into_any_element(),
        )
    }

    fn render_resize_handle(&self, which: Resizing, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active = self.resizing == Some(which);
        let id = match which {
            Resizing::Sidebar => "resize-sidebar",
            Resizing::Dock => "resize-dock",
        };
        h_flex()
            .id(id)
            .group(id)
            .flex_none()
            .w(HANDLE_WIDTH)
            .h_full()
            .justify_center()
            .cursor_col_resize()
            .child(
                div()
                    .h_full()
                    .w(px(1.))
                    .bg(theme.border)
                    .group_hover(id, |s| s.w(px(2.)).bg(theme.ring.opacity(0.7)))
                    .when(active, |s| s.w(px(2.)).bg(theme.ring)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.resizing = Some(which);
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    fn render_header(&self, visible: usize, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let new_button = |id: &'static str, icon: IconName, label: &'static str, tooltip: &'static str, kind: CardKind, cx: &mut Context<Self>| {
            Button::new(id)
                .outline()
                .icon(icon)
                .when(!compact, |b| b.label(label))
                .tooltip(tooltip)
                .on_click(cx.listener(move |this, _, window, cx| this.create(kind, None, window, cx)))
        };
        let open_todos = self.stats.open_todos;

        v_flex()
            .w_full()
            .max_w(px(LIST_MAX_WIDTH))
            .mx_auto()
            .gap_4()
            .px(px(LIST_PADDING))
            .pt_6()
            .pb_2()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(160.))
                            .child(div().text_2xl().font_semibold().truncate().child(self.filter.title()))
                            .child(
                                h_flex()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(match visible {
                                        0 => "No cards".to_string(),
                                        1 => "1 card".to_string(),
                                        n => format!("{n} cards"),
                                    })
                                    .when(open_todos > 0 && self.filter != Filter::OpenTodos, |s| {
                                        s.child(format!(" · {open_todos} open todos"))
                                    })
                                    .when_some(
                                        match &self.filter {
                                            Filter::Saved(saved) => self
                                                .queries
                                                .iter()
                                                .position(|q| q == saved)
                                                .map(|ix| (ix, saved.query.clone())),
                                            _ => None,
                                        },
                                        |s, (ix, query)| {
                                            s.child(div().truncate().child(format!(" · {query}")))
                                                .child(
                                                    div()
                                                        .id("edit-active-query")
                                                        .ml_2()
                                                        .flex_none()
                                                        .cursor_pointer()
                                                        .text_color(theme.link)
                                                        .hover(|s| s.underline())
                                                        .child("Edit")
                                                        .on_click(cx.listener(move |this, _, window, cx| {
                                                            this.open_query_form(Some(ix), window, cx)
                                                        })),
                                                )
                                        },
                                    ),
                            ),
                    )
                    .child(
                        Button::new("paste")
                            .ghost()
                            .icon(IconName::ClipboardPaste)
                            .when(!compact, |b| b.label("Paste"))
                            .tooltip("Paste clipboard as snippet (Ctrl+Shift+V)")
                            .on_click(cx.listener(|this, _, window, cx| this.paste_snippet(window, cx))),
                    )
                    .child(new_button("new-snippet", IconName::Code, "Snippet", "New snippet (Ctrl+Shift+N)", CardKind::Snippet, cx))
                    .child(new_button("new-todo", IconName::ListTodo, "Todo", "New todo (Ctrl+T)", CardKind::Todo, cx))
                    .child(new_button("new-note", IconName::StickyNote, "Note", "New note (Ctrl+N)", CardKind::Note, cx)),
            )
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground))
                    .suffix(kbd("ctrl-k"))
                    .cleanable(true),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.rows.is_empty() {
            return self.render_empty(cx);
        }
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                list(
                    self.list_state.clone(),
                    cx.processor(|this, ix: usize, _, cx| this.render_row(ix, cx)),
                )
                .size_full()
                .pb(px(96.)),
            )
            .vertical_scrollbar(&self.list_state)
            .into_any_element()
    }

    /// One list row, centered in the reading column.
    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.rows.get(ix) {
            Some(Row::Section { day: None, count }) => {
                self.render_section("Pinned", Some(IconName::Pin), *count, cx)
            }
            Some(Row::Section { day: Some(day), count }) => {
                self.render_section(&day_label(*day), None, *count, cx)
            }
            Some(Row::Card(id)) => match self.card(id) {
                Some(card) => self.render_card(card, cx),
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(LIST_MAX_WIDTH))
                    .px(px(LIST_PADDING))
                    .pb_3()
                    .child(content),
            )
            .into_any_element()
    }

    fn render_section(&self, label: &str, icon: Option<IconName>, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        h_flex()
            .gap_2()
            .pt_4()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .when_some(icon, |s, icon| s.child(Icon::new(icon).xsmall()))
            .child(div().font_semibold().text_color(theme.foreground.opacity(0.75)).child(label.to_string()))
            .child(div().flex_1().h_px().bg(theme.border))
            .child(count.to_string())
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let searching = !self.query.trim().is_empty();
        let open_view = self.filter == Filter::OpenTodos;
        let (icon, title, hint) = if searching {
            (IconName::Search, "Nothing found", "Try other words, a #tag or is:todo.")
        } else if open_view {
            (IconName::SquareCheck, "All done!", "No unfinished todos left.")
        } else {
            (
                IconName::Sparkles,
                "Nothing here yet",
                "Capture a thought, a todo or a pasted snippet.",
            )
        };
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .py_24()
            .child(
                div()
                    .size_16()
                    .rounded_full()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).large().text_color(theme.muted_foreground)),
            )
            .child(div().text_lg().font_semibold().child(title))
            .child(div().text_sm().text_color(theme.muted_foreground).child(hint))
            .when(!searching && !open_view, |this| {
                this.child(
                    h_flex()
                        .gap_4()
                        .pt_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(h_flex().gap_1p5().child(kbd("ctrl-n")).child("note"))
                        .child(h_flex().gap_1p5().child(kbd("ctrl-t")).child("todo"))
                        .child(h_flex().gap_1p5().child(kbd("ctrl-shift-v")).child("paste")),
                )
            })
            .into_any_element()
    }

    fn render_card(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let accent = accent(card.color);
        let tinted = card.color != CardColor::Slate;
        let selected = self.selected.as_deref() == Some(card.id.as_str());
        let done = card.is_done_todo();
        let bg = if tinted {
            accent.mix(theme.background, 0.06)
        } else {
            theme.secondary.mix(theme.background, 0.45)
        };
        let border = if selected {
            accent
        } else if tinted {
            accent.opacity(0.2)
        } else {
            theme.border
        };
        let id = card.id.clone();
        let group: SharedString = format!("card-{}", card.id).into();

        let badge = |text: String, color: Hsla| {
            div()
                .flex_none()
                .px_1p5()
                .py(px(1.))
                .rounded(px(4.))
                .text_xs()
                .bg(color.opacity(0.14))
                .text_color(color)
                .child(text)
        };

        let header = h_flex()
            .gap_2p5()
            .items_center()
            .child(
                div()
                    .flex_none()
                    .size_6()
                    .rounded(px(6.))
                    .bg(accent.opacity(0.16))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(kind_icon(card.kind)).xsmall().text_color(accent)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_semibold()
                    .truncate()
                    .when(done, |s| s.line_through())
                    .text_color(if card.has_title() || card.kind != CardKind::Snippet {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .child(card.display_title()),
            )
            .when(card.kind == CardKind::Snippet, |s| {
                s.child(badge(language_label(card.language()).to_string(), accent))
            })
            .when(done, |s| s.child(badge("Done".into(), theme.success)))
            .child(
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
                    .child(self.card_action(&format!("copy-{}", card.id), IconName::Copy, "Copy", cx, {
                        let id = id.clone();
                        move |this, window, cx| this.copy_card(&id, window, cx)
                    }))
                    .child(self.card_action(&format!("pin-{}", card.id), IconName::Pin, "Pin", cx, {
                        let id = id.clone();
                        move |this, _, cx| this.update_card(&id, cx, |c| c.pinned = !c.pinned)
                    }))
                    .child(self.card_action(&format!("del-{}", card.id), IconName::Trash, "Delete", cx, {
                        let id = id.clone();
                        move |this, window, cx| this.delete(&id, window, cx)
                    })),
            )
            .when(card.pinned, |s| {
                s.child(Icon::new(IconName::Pin).xsmall().text_color(accent))
            });

        let content: Option<AnyElement> = match card.kind {
            CardKind::Note => {
                // Images show as thumbnails below the text.
                let body = strip_images(&card.body);
                let text = if card.has_title() {
                    body.trim().to_string()
                } else {
                    // The first line already serves as title.
                    let mut lines = body.trim().lines();
                    lines.next();
                    lines.collect::<Vec<_>>().join("\n").trim().to_string()
                };
                let root = self.vault.as_ref().map(|v| v.root().to_path_buf()).unwrap_or_default();
                let text = (!text.is_empty()).then(|| {
                    div()
                        .text_sm()
                        .text_color(theme.foreground.opacity(0.8))
                        .child(
                            TextView::markdown(SharedString::from(format!("md-{}", card.id)), text)
                                .style(card_markdown_style())
                                .selectable(false)
                                .max_lines(12)
                                .on_link_click(move |url, _, window, cx| open_link(&root, url, window, cx)),
                        )
                });
                let thumbnails = self.render_thumbnails(card, cx);
                let files = self.render_file_chips(card, false, cx);
                (text.is_some() || thumbnails.is_some() || files.is_some()).then(|| {
                    v_flex()
                        .gap_3()
                        .children(text)
                        .children(thumbnails)
                        .children(files)
                        .into_any_element()
                })
            }
            CardKind::Snippet => (!card.body.trim().is_empty()).then(|| {
                let total = card.body.lines().count();
                let code: String = card
                    .body
                    .lines()
                    .take(SNIPPET_PREVIEW_LINES)
                    .collect::<Vec<_>>()
                    .join("\n");
                v_flex()
                    .gap_1()
                    .text_sm()
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("code-{}", card.id)),
                            fenced(&code, card.language()),
                        )
                        .selectable(false),
                    )
                    .when(total > SNIPPET_PREVIEW_LINES, |s| {
                        s.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("… {} more lines", total - SNIPPET_PREVIEW_LINES)),
                        )
                    })
                    .into_any_element()
            }),
            CardKind::Todo => {
                let (done_count, total) = card.progress();
                let shown = 10;
                // Unfinished tasks first so open work is always visible.
                let mut items: Vec<(usize, &TodoItem)> = card.items.iter().enumerate().collect();
                items.sort_by_key(|(_, item)| item.done);
                let root = self.vault.as_ref().map(|v| v.root().to_path_buf()).unwrap_or_default();
                let description = card.body.trim();
                let description = (!description.is_empty()).then(|| {
                    div()
                        .pb_1p5()
                        .text_sm()
                        .text_color(theme.foreground.opacity(0.8))
                        .child(
                            TextView::markdown(SharedString::from(format!("md-{}", card.id)), description.to_string())
                                .style(card_markdown_style())
                                .selectable(false)
                                .max_lines(4)
                                .on_link_click(move |url, _, window, cx| open_link(&root, url, window, cx)),
                        )
                });
                Some(
                    v_flex()
                        .gap_1()
                        .children(description)
                        .children(
                            items
                                .into_iter()
                                .take(shown)
                                .map(|(ix, item)| self.render_check_row(&card.id, ix, item, accent, false, cx))
                                .collect::<Vec<_>>(),
                        )
                        .when(total > shown, |s| {
                            s.child(
                                div()
                                    .pl_7()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("+{} more", total - shown)),
                            )
                        })
                        .when(total > 0, |s| {
                            s.child(
                                h_flex()
                                    .pt_1p5()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .h(px(4.))
                                            .rounded_full()
                                            .bg(theme.foreground.opacity(0.08))
                                            .child(
                                                div()
                                                    .h_full()
                                                    .rounded_full()
                                                    .bg(if done { theme.success } else { accent })
                                                    .w(relative(done_count as f32 / total as f32)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{done_count}/{total}")),
                                    ),
                            )
                        })
                        .when(total == 0, |s| {
                            s.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("No tasks yet"),
                            )
                        })
                        .into_any_element(),
                )
            }
        };

        let created = card.created.with_timezone(&Local).format("%H:%M").to_string();
        let footer = h_flex()
            .gap_1()
            .flex_wrap()
            .items_center()
            .children(self.render_project_chip(card, cx))
            .children(
                card.tags
                    .iter()
                    .map(|tag| self.render_tag_chip(&card.id, tag, cx))
                    .collect::<Vec<_>>(),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground.opacity(0.8))
                    .child(if card.updated - card.created > chrono::TimeDelta::minutes(1) {
                        format!("{created} · edited {}", relative_time(card.updated))
                    } else {
                        created
                    }),
            );

        v_flex()
            .id(SharedString::from(format!("card-{}", card.id)))
            .group(group)
            .w_full()
            .gap_3()
            .px_5()
            .py_4()
            .bg(bg)
            .border_1()
            .border_color(border)
            .rounded(theme.radius_lg)
            .shadow_sm()
            .cursor_pointer()
            .overflow_hidden()
            .when(done, |s| s.opacity(0.65))
            .hover(|s| s.border_color(accent.opacity(0.55)).shadow_md())
            .child(
                // Post-it color strip.
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(if tinted { accent } else { gpui_kit::transparent_black() }),
            )
            .child(header)
            .children(content)
            .child(footer)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.open(&id, window, cx);
                // The popup may not be on screen yet when the second click lands.
                if event.click_count() >= 2 {
                    this.keep_selected(cx);
                }
            }))
            .into_any_element()
    }

    fn card_action(
        &self,
        id: &str,
        icon: IconName,
        tooltip: &'static str,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        div()
            .id(SharedString::from(id.to_string()))
            .size_6()
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.muted_foreground)
            .hover(|s| s.bg(theme.foreground.opacity(0.08)).text_color(theme.foreground))
            .child(Icon::new(icon).xsmall())
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    on_click(this, window, cx);
                }),
            )
            .into_any_element()
    }

    fn render_check_row(
        &self,
        card_id: &str,
        ix: usize,
        item: &TodoItem,
        accent: Hsla,
        removable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let toggle_id = card_id.to_string();
        let remove_id = card_id.to_string();
        let group: SharedString = format!("item-{card_id}-{ix}").into();
        h_flex()
            .id(SharedString::from(format!("check-{card_id}-{ix}-{removable}")))
            .group(group.clone())
            .gap_2p5()
            .items_start()
            .py_0p5()
            .rounded(px(4.))
            .child(
                div()
                    .mt(px(2.))
                    .flex_none()
                    .size(px(16.))
                    .rounded(px(4.))
                    .border_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(item.done, |s| s.bg(accent).border_color(accent))
                    .when(!item.done, |s| {
                        s.border_color(theme.muted_foreground.opacity(0.6))
                            .hover(|s| s.border_color(accent))
                    })
                    .when(item.done, |s| {
                        s.child(Icon::new(IconName::Check).size(px(12.)).text_color(gpui_kit::black()))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .line_height(relative(1.45))
                    .when(item.done, |s| s.line_through().text_color(theme.muted_foreground))
                    .when(!item.done, |s| s.text_color(theme.foreground.opacity(0.9)))
                    .child(item.text.clone()),
            )
            .when(removable, |s| {
                s.child(
                    div()
                        .id(SharedString::from(format!("rm-{card_id}-{ix}")))
                        .flex_none()
                        .size_5()
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover(group, |s| s.opacity(1.))
                        .text_color(theme.muted_foreground)
                        .hover(|s| s.text_color(theme.danger).bg(theme.danger.opacity(0.12)))
                        .child(Icon::new(IconName::X).xsmall())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.update_card(&remove_id, cx, |c| {
                                    if ix < c.items.len() {
                                        c.items.remove(ix);
                                    }
                                });
                            }),
                        ),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.update_card(&toggle_id, cx, |c| {
                        if let Some(item) = c.items.get_mut(ix) {
                            item.done = !item.done;
                        }
                    });
                }),
            )
            .into_any_element()
    }

    fn render_tag_chip(&self, card_id: &str, tag: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let filter_tag = tag.to_string();
        div()
            .id(SharedString::from(format!("chip-{card_id}-{tag}")))
            .px_2()
            .py(px(1.))
            .rounded_full()
            .text_xs()
            .bg(theme.foreground.opacity(0.06))
            .border_1()
            .border_color(theme.foreground.opacity(0.08))
            .text_color(theme.muted_foreground)
            .hover(|s| s.text_color(theme.foreground).bg(theme.foreground.opacity(0.12)))
            .child(format!("#{tag}"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_filter(Filter::Tag(filter_tag.clone()), window, cx);
                }),
            )
            .into_any_element()
    }

    /// Project or component of a card: links to its filter.
    fn render_project_chip(&self, card: &Card, cx: &mut Context<Self>) -> Option<AnyElement> {
        let project = card.project.clone()?;
        let theme = cx.theme().clone();
        // Links to the deepest level the card has.
        let filter = match (card.component.clone(), card.feature.clone()) {
            (Some(component), Some(feature)) => Filter::Feature(project, component, feature),
            (Some(component), None) => Filter::Component(project, component),
            _ => Filter::Project(project),
        };
        let label = filter.title();
        Some(
            h_flex()
                .id(SharedString::from(format!("project-{}", card.id)))
                .gap_1()
                .px_2()
                .py(px(1.))
                .rounded_full()
                .text_xs()
                .bg(theme.foreground.opacity(0.06))
                .border_1()
                .border_color(theme.foreground.opacity(0.08))
                .text_color(theme.muted_foreground)
                .hover(|s| s.text_color(theme.foreground).bg(theme.foreground.opacity(0.12)))
                .child(Icon::new(IconName::Folder).size(px(11.)))
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.set_filter(filter.clone(), window, cx);
                    }),
                )
                .into_any_element(),
        )
    }

    /// Text input for a project or component, with a menu of the existing ones.
    #[allow(clippy::too_many_arguments)]
    fn render_project_field(
        &self,
        id: &'static str,
        input: &Entity<InputState>,
        icon: IconName,
        none_label: &'static str,
        current: Option<String>,
        options: Vec<String>,
        set: fn(&mut Self, Option<String>, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let this = cx.entity().downgrade();
        let picker = Button::new(SharedString::from(format!("{id}-picker")))
            .ghost()
            .xsmall()
            .icon(IconName::ChevronsUpDown)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(320.));
                let pick = |value: Option<String>| {
                    let this = this.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let value = value.clone();
                        let _ = this.update(cx, |this, cx| set(this, value, window, cx));
                    }
                };
                menu = menu.item(
                    PopupMenuItem::new(none_label)
                        .checked(current.is_none())
                        .on_click(pick(None)),
                );
                if !options.is_empty() {
                    menu = menu.separator();
                }
                for option in &options {
                    menu = menu.item(
                        PopupMenuItem::new(option.clone())
                            .checked(current.as_ref() == Some(option))
                            .on_click(pick(Some(option.clone()))),
                    );
                }
                menu
            });
        div()
            .w(px(150.))
            .child(
                Input::new(input)
                    .xsmall()
                    .appearance(false)
                    .prefix(Icon::new(icon).xsmall().text_color(theme.muted_foreground))
                    .suffix(picker),
            )
            .into_any_element()
    }

    /// A small segmented control.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: Vec<(T, Option<IconName>, &'static str)>,
        active: T,
        accent: Hsla,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, T, &mut Window, &mut Context<Self>) + Clone + 'static,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        h_flex()
            .p(px(3.))
            .gap(px(2.))
            .rounded(theme.radius)
            .bg(theme.muted)
            .children(
                options
                    .into_iter()
                    .map(|(value, icon, label)| {
                        let is_active = value == active;
                        let on_select = on_select.clone();
                        h_flex()
                            .id(SharedString::from(format!("{id}-{label}")))
                            .gap_1p5()
                            .px_2p5()
                            .py_1()
                            .rounded(theme.radius - px(2.))
                            .text_xs()
                            .font_medium()
                            .cursor_pointer()
                            .text_color(if is_active { theme.foreground } else { theme.muted_foreground })
                            .when(is_active, |s| s.bg(theme.background).shadow_sm())
                            .when(!is_active, |s| s.hover(|s| s.text_color(theme.foreground)))
                            .when_some(icon, |s, icon| {
                                s.child(Icon::new(icon).xsmall().when(is_active, |i| i.text_color(accent)))
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, window, cx| on_select(this, value, window, cx)))
                    })
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    fn render_language_picker(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let current = card.language.clone();
        let detected = detect_language(&card.body);
        let label = match &current {
            Some(lang) => language_label(lang).to_string(),
            None => format!("Auto · {}", language_label(detected)),
        };
        let this = cx.entity().downgrade();
        Button::new("language")
            .ghost()
            .xsmall()
            .icon(IconName::Code)
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.));
                let pick = |language: Option<String>| {
                    let this = this.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let language = language.clone();
                        let _ = this.update(cx, |this, cx| this.set_language(language, cx));
                    }
                };
                menu = menu
                    .item(
                        PopupMenuItem::new(format!("Auto-detect ({})", language_label(detected)))
                            .checked(current.is_none())
                            .on_click(pick(None)),
                    )
                    .separator();
                for (name, label) in LANGUAGES {
                    menu = menu.item(
                        PopupMenuItem::new(*label)
                            .checked(current.as_deref() == Some(*name))
                            .on_click(pick(Some(name.to_string()))),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_editor(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let accent = accent(card.color);
        let id = card.id.clone();

        let kinds = self.segmented(
            "kind",
            CardKind::ALL
                .iter()
                .map(|k| (*k, Some(kind_icon(*k)), k.label()))
                .collect(),
            card.kind,
            accent,
            cx,
            |this, kind, window, cx| this.set_kind(kind, window, cx),
        );

        let colors = h_flex().gap_1p5().children(CardColor::ALL.map(|color| {
            let active = card.color == color;
            div()
                .id(SharedString::from(format!("color-{color:?}")))
                .size(px(18.))
                .rounded_full()
                .cursor_pointer()
                .border_2()
                .border_color(if active { theme.foreground } else { gpui_kit::transparent_black() })
                .p(px(2.))
                .child(div().size_full().rounded_full().bg(self::accent(color)))
                .hover(|s| s.border_color(theme.foreground.opacity(0.5)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_selected(cx, |card| card.color = color)
                }))
        }));

        let tags = h_flex()
            .gap_1p5()
            .flex_wrap()
            .items_center()
            .children(
                card.tags
                    .iter()
                    .map(|tag| {
                        let tag_owned = tag.clone();
                        h_flex()
                            .gap_1()
                            .pl_2()
                            .pr_1()
                            .py(px(2.))
                            .rounded_full()
                            .text_xs()
                            .bg(accent.opacity(0.14))
                            .text_color(theme.foreground.opacity(0.9))
                            .child(format!("#{tag}"))
                            .child(
                                div()
                                    .id(SharedString::from(format!("tag-x-{tag}")))
                                    .rounded_full()
                                    .p(px(1.))
                                    .cursor_pointer()
                                    .text_color(theme.muted_foreground)
                                    .hover(|s| s.text_color(theme.foreground).bg(theme.foreground.opacity(0.1)))
                                    .child(Icon::new(IconName::X).size(px(11.)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let tag = tag_owned.clone();
                                        this.update_selected(cx, |card| card.tags.retain(|t| *t != tag))
                                    })),
                            )
                    })
                    .collect::<Vec<_>>(),
            )
            .child(
                div().w(px(140.)).child(
                    Input::new(&self.tag_input)
                        .xsmall()
                        .appearance(false)
                        .prefix(Icon::new(IconName::Tag).xsmall().text_color(theme.muted_foreground)),
                ),
            );

        let project = h_flex()
            .gap_2()
            .flex_wrap()
            .child(self.render_project_field(
                "project",
                &self.project_input,
                IconName::Folder,
                "No project",
                card.project.clone(),
                self.stats.projects.keys().cloned().collect(),
                Self::set_project,
                cx,
            ))
            .when(card.project.is_some(), |s| {
                let components = card
                    .project
                    .as_ref()
                    .and_then(|p| self.stats.projects.get(p))
                    .map(|p| p.components.keys().cloned().collect())
                    .unwrap_or_default();
                s.child(Icon::new(IconName::ChevronRight).xsmall().text_color(theme.muted_foreground))
                    .child(self.render_project_field(
                        "component",
                        &self.component_input,
                        IconName::Component,
                        "No component",
                        card.component.clone(),
                        components,
                        Self::set_component,
                        cx,
                    ))
            })
            .when_some(
                card.project.as_ref().zip(card.component.as_ref()),
                |s, (project, component)| {
                    let features = self
                        .stats
                        .component(project, component)
                        .map(|c| c.features.keys().cloned().collect())
                        .unwrap_or_default();
                    s.child(Icon::new(IconName::ChevronRight).xsmall().text_color(theme.muted_foreground))
                        .child(self.render_project_field(
                            "feature",
                            &self.feature_input,
                            IconName::Flag,
                            "No feature",
                            card.feature.clone(),
                            features,
                            Self::set_feature,
                            cx,
                        ))
                },
            );

        let body: AnyElement = match card.kind {
            CardKind::Todo => {
                let (done, total) = card.progress();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .child(div().text_xs().text_color(theme.muted_foreground).child("DESCRIPTION"))
                    .child(
                        v_flex()
                            .flex_none()
                            .h(px(TODO_DESCRIPTION_HEIGHT))
                            .capture_action(cx.listener(Self::on_body_enter))
                            .child(Editor::new(&self.body).h(relative(1.))),
                    )
                    .child(
                        h_flex()
                            .pt_2()
                            .justify_between()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("TASKS")
                            .when(total > 0, |s| s.child(format!("{done} of {total} done"))),
                    )
                    .child(
                        Input::new(&self.item_input)
                            .prefix(Icon::new(IconName::Plus).small().text_color(theme.muted_foreground)),
                    )
                    .child(
                        v_flex()
                            .id("todo-list")
                            .flex_1()
                            .min_h_0()
                            .gap_1()
                            .overflow_y_scrollbar()
                            .children(
                                card.items
                                    .iter()
                                    .enumerate()
                                    .map(|(ix, item)| self.render_check_row(&card.id, ix, item, accent, true, cx))
                                    .collect::<Vec<_>>(),
                            ),
                    )
                    .when(done > 0, |s| {
                        s.child(
                            Button::new("clear-done")
                                .ghost()
                                .xsmall()
                                .label("Clear completed")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_selected(cx, |c| c.items.retain(|i| !i.done))
                                })),
                        )
                    })
                    .into_any_element()
            }
            CardKind::Note if self.preview => v_flex()
                .id("note-preview")
                .flex_1()
                .min_h_0()
                .px_1()
                .on_double_click(cx.listener(|this, _, window, cx| this.set_preview(false, window, cx)))
                .overflow_y_scrollbar()
                .child(match &self.vault {
                    Some(vault) => vault_markdown("editor-preview", card.body.clone(), vault.root())
                        .selectable(true)
                        .into_any_element(),
                    None => TextView::markdown("editor-preview", card.body.clone())
                        .selectable(true)
                        .into_any_element(),
                })
                .into_any_element(),
            CardKind::Note | CardKind::Snippet => v_flex()
                .flex_1()
                .min_h_0()
                .capture_action(cx.listener(Self::on_body_enter))
                .child(Editor::new(&self.body).h(relative(1.)))
                .into_any_element(),
        };

        let body_toolbar: Option<AnyElement> = match card.kind {
            CardKind::Note => Some(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if self.preview {
                                "Double-click to edit"
                            } else {
                                "Markdown · drop or paste files to attach"
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("attach")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Paperclip)
                                    .label("Attach")
                                    .tooltip("Embed images or files")
                                    .on_click(cx.listener(|this, _, window, cx| this.prompt_attach(window, cx))),
                            )
                            .child(self.segmented(
                                "mode",
                                vec![(false, Some(IconName::NotebookPen), "Write"), (true, Some(IconName::BookOpen), "Preview")],
                                self.preview,
                                accent,
                                cx,
                                |this, preview, window, cx| this.set_preview(preview, window, cx),
                            )),
                    )
                    .into_any_element(),
            ),
            CardKind::Snippet => Some(
                h_flex()
                    .justify_between()
                    .child(self.render_language_picker(card, cx))
                    .child(
                        Button::new("copy-code")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .label("Copy")
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |this, _, window, cx| this.copy_card(&id, window, cx))
                            }),
                    )
                    .into_any_element(),
            ),
            CardKind::Todo => None,
        };

        let words = card.body.split_whitespace().count();
        let stats = match card.kind {
            CardKind::Note => format!("{words} words"),
            CardKind::Snippet => format!("{} lines · {} chars", card.body.lines().count(), card.body.chars().count()),
            CardKind::Todo => format!("{} tasks", card.items.len()),
        };

        v_flex()
            .id("editor")
            .relative()
            .size_full()
            // gpui clips to rectangles, so the surface itself carries the rounding.
            .rounded(modal_radius(&theme))
            .border_1()
            .border_color(theme.border)
            .bg(theme.background.mix(theme.secondary, 0.35))
            .capture_action(cx.listener(Self::on_paste))
            .when(card.kind == CardKind::Note, |s| {
                // Other kinds let the drop through to the list, which makes a new note.
                s.drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(accent))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        this.attach_files(paths.paths(), window, cx)
                    }))
            })
            .child(
                // Accent line, kept clear of the rounded corners.
                div()
                    .absolute()
                    .top_0()
                    .left(modal_radius(&theme))
                    .right(modal_radius(&theme))
                    .h(px(2.))
                    .rounded_b(px(2.))
                    .bg(accent),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .child(kinds)
                    .child(div().flex_1())
                    .child(
                        Button::new("pin")
                            .ghost()
                            .small()
                            .icon(if card.pinned { IconName::PinOff } else { IconName::Pin })
                            .tooltip(if card.pinned { "Unpin (Ctrl+P)" } else { "Pin (Ctrl+P)" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_selected(cx, |c| c.pinned = !c.pinned)
                            })),
                    )
                    .child(
                        Button::new("copy")
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .tooltip("Copy content")
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |this, _, window, cx| this.copy_card(&id, window, cx))
                            }),
                    )
                    .child(
                        Button::new("delete")
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip("Delete card")
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |this, _, window, cx| this.delete(&id, window, cx))
                            }),
                    )
                    .child(
                        Button::new("minimize")
                            .ghost()
                            .small()
                            .icon(IconName::Minus)
                            .tooltip("Keep in sidebar and hide (Ctrl+M)")
                            .on_click(cx.listener(|this, _, window, cx| this.minimize(window, cx))),
                    )
                    .child(
                        Button::new("close")
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .tooltip("Close and remove from sidebar")
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |this, _, window, cx| this.close_tab(&id, window, cx))
                            }),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_5()
                    .pb_4()
                    .gap_3()
                    .child(
                        Input::new(&self.title)
                            .appearance(false)
                            .large()
                            .text_xl()
                            .font_semibold(),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .flex_wrap()
                            .gap_2()
                            .child(colors)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "Created {}",
                                        card.created.with_timezone(&Local).format("%b %-d, %Y %H:%M")
                                    )),
                            ),
                    )
                    .child(project)
                    .child(tags)
                    .child(div().h_px().w_full().bg(theme.border))
                    .children(body_toolbar)
                    .child(body)
                    .children(
                        (card.kind == CardKind::Note)
                            .then(|| self.render_file_chips(card, true, cx))
                            .flatten(),
                    ),
            )
            .child(
                h_flex()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(stats)
                    .child(div().flex_1())
                    .child(format!("Edited {} · saved", relative_time(card.updated))),
            )
            .into_any_element()
    }

    fn render_undo(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let undo = self.undo.as_ref()?;
        let theme = cx.theme().clone();
        Some(
            div()
                .absolute()
                .bottom_5()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    h_flex()
                        .gap_3()
                        .pl_4()
                        .pr_1p5()
                        .py_1p5()
                        .rounded(theme.radius_lg)
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .text_sm()
                        .child(Icon::new(IconName::Trash).small().text_color(theme.muted_foreground))
                        .child(format!("Deleted “{}”", truncate(&undo.card.display_title(), 32)))
                        .child(
                            Button::new("undo")
                                .small()
                                .label("Undo")
                                .on_click(cx.listener(|this, _, window, cx| this.undo_delete(window, cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl ZettelApp {
    /// Right-hand dock listing minimized cards for quick reopening.
    fn render_dock(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut cards: Vec<Card> = self
            .ui
            .minimized
            .iter()
            .filter_map(|id| self.card(id).cloned())
            .collect();
        let kept_count = cards.len();
        // The open preview is listed after the kept cards, in italics.
        if let Some(card) = self.selected_card().filter(|c| !self.is_kept(&c.id)) {
            cards.push(card.clone());
        }
        let theme = cx.theme().clone();

        let items = cards
            .iter()
            .map(|card| {
                let accent = accent(card.color);
                let open_id = card.id.clone();
                let remove_id = card.id.clone();
                let group: SharedString = format!("dock-{}", card.id).into();
                let active = self.selected.as_deref() == Some(card.id.as_str());
                let is_preview = !self.is_kept(&card.id);
                let subtitle = match card.kind {
                    CardKind::Todo => {
                        let (done, total) = card.progress();
                        format!("{done}/{total} done")
                    }
                    CardKind::Snippet => language_label(card.language()).to_string(),
                    CardKind::Note => {
                        let line = card
                            .body
                            .lines()
                            .map(|l| l.trim().trim_start_matches(['#', '-', '>', '*']).trim())
                            .filter(|l| !l.is_empty())
                            .nth(usize::from(!card.has_title()))
                            .unwrap_or("Note")
                            .replace(['*', '_', '`'], "");
                        truncate(&line, 80)
                    }
                };
                h_flex()
                    .id(SharedString::from(format!("dock-item-{}", card.id)))
                    .group(group.clone())
                    .gap_2p5()
                    .pl_2p5()
                    .pr_1p5()
                    .py_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(if active { accent } else { theme.border })
                    .bg(theme.background.mix(accent, if active { 0.84 } else { 0.94 }))
                    .when(is_preview, |s| s.border_dashed())
                    .cursor_pointer()
                    .hover(|s| s.border_color(accent.opacity(0.6)).bg(theme.background.mix(accent, 0.88)))
                    .child(
                        div()
                            .flex_none()
                            .size_7()
                            .rounded(px(6.))
                            .bg(accent.opacity(0.16))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(kind_icon(card.kind)).small().text_color(accent)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .when(is_preview, |s| s.italic())
                                    .child(card.display_title()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(if is_preview {
                                        format!("Preview · {subtitle}")
                                    } else {
                                        subtitle
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("dock-x-{}", card.id)))
                            .flex_none()
                            .size_6()
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .when(!active, |s| s.opacity(0.).group_hover(group, |s| s.opacity(1.)))
                            .hover(|s| s.text_color(theme.foreground).bg(theme.foreground.opacity(0.1)))
                            .child(Icon::new(IconName::X).xsmall())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(&remove_id, window, cx);
                                }),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.open(&open_id, window, cx)))
            })
            .collect::<Vec<_>>();

            v_flex()
                .w(px(self.ui.dock_width))
                .flex_none()
                .h_full()
                .bg(theme.sidebar)
                .child(
                    h_flex()
                        .gap_2()
                        .px_4()
                        .pt_4()
                        .pb_2()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground.opacity(0.8))
                        .child("OPEN CARDS")
                        .child(
                            div()
                                .px_1p5()
                                .rounded_full()
                                .bg(theme.muted)
                                .text_color(theme.muted_foreground)
                                .child(kept_count.to_string()),
                        )
                        .child(div().flex_1())
                        .when(kept_count > 0, |s| s.child(
                            Button::new("dock-clear")
                                .ghost()
                                .xsmall()
                                .label("Clear")
                                .tooltip("Remove all cards from the sidebar")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.hide(window, cx);
                                    this.ui.minimized.clear();
                                    this.save_settings();
                                    cx.notify();
                                })),
                        )),
                )
                .when(cards.is_empty(), |s| {
                    s.child(
                        v_flex()
                            .mx_3()
                            .p_4()
                            .gap_2()
                            .items_center()
                            .rounded(theme.radius)
                            .border_1()
                            .border_dashed()
                            .border_color(theme.border)
                            .text_center()
                            .child(Icon::new(IconName::Layers).text_color(theme.muted_foreground))
                            .child(div().text_sm().font_medium().child("No open cards"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Double-click a card to keep it here for quick access."),
                            ),
                    )
                })
                .child(
                    v_flex()
                        .id("dock-scroll")
                        .flex_1()
                        .min_h_0()
                        .px_3()
                        .pb_3()
                        .gap_2()
                        .overflow_y_scrollbar()
                        .children(items),
                )
                .into_any_element()
    }

    /// The card editor as a centered popup over the library.
    fn render_modal(&self, card: &Card, area: Size<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let width = MODAL_MAX.0.min(f32::from(area.width) - 48.).max(320.);
        let height = MODAL_MAX.1.min(f32::from(area.height) - 48.).max(320.);
        let animation = || Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint());

        div()
            .id("modal-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::black().opacity(0.55))
            .occlude()
            // Capture runs before the popup can stop the event, so the second
            // click of a double click is seen wherever it lands.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if this.is_double_click_on_open(event) {
                    this.keep_selected(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if !this.is_double_click_on_open(event) {
                        this.hide(window, cx);
                    }
                }),
            )
            .child(
                div()
                    .id("modal")
                    .w(px(width))
                    .h(px(height))
                    .rounded(modal_radius(&theme))
                    .shadow_2xl()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.render_editor(card, cx))
                    .with_animation(
                        SharedString::from(format!("modal-in-{}", card.id)),
                        animation(),
                        |el, t| el.opacity(t).mt(px(18. * (1. - t))),
                    ),
            )
            .with_animation(
                SharedString::from(format!("backdrop-in-{}", card.id)),
                animation(),
                |el, t| el.bg(gpui_kit::black().opacity(0.55 * t)),
            )
            .into_any_element()
    }
}

impl ZettelApp {
    fn render_title_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        TitleBar::new()
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .size_5()
                            .rounded(px(5.))
                            .bg(accent(CardColor::Yellow))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::NotebookPen)
                                    .size(px(12.))
                                    .text_color(gpui_kit::black()),
                            ),
                    )
                    .child(div().font_semibold().child("Zettelkasten"))
                    .when(self.vault.is_some(), |s| {
                        s.child(div().text_color(cx.theme().muted_foreground).child("/"))
                            .child(self.render_vault_menu(cx))
                    }),
            )
            .child(div().pr_2().child(self.render_settings_menu(cx)))
            .into_any_element()
    }

    /// App settings in the title bar.
    fn render_settings_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        let current = self.config.theme;
        Button::new("settings")
            .ghost()
            .xsmall()
            .icon(IconName::Settings)
            .tooltip("Settings")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label("Theme");
                for theme in ThemePreference::ALL {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(theme.label())
                            .checked(current == theme)
                            .on_click(move |_, window, cx| {
                                let _ = this.update(cx, |this, cx| this.set_theme(theme, window, cx));
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// Vault switcher in the title bar.
    fn render_vault_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        let current = self.vault.as_ref().map(|v| v.root().to_path_buf());
        let name = current.as_deref().map(vault_name).unwrap_or_default();
        let recents: Vec<PathBuf> = self
            .config
            .recent_vaults
            .iter()
            .filter(|p| p.is_dir())
            .cloned()
            .collect();

        Button::new("vault-menu")
            .ghost()
            .xsmall()
            .icon(IconName::Vault)
            .label(name)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let action = |f: fn(&mut ZettelApp, &mut Window, &mut Context<ZettelApp>)| {
                    let this = this.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| f(this, window, cx));
                    }
                };
                let mut menu = menu.label("Vaults");
                for path in &recents {
                    let this = this.clone();
                    let target = path.clone();
                    menu = menu.item(
                        PopupMenuItem::new(vault_name(path))
                            .checked(current.as_ref() == Some(path))
                            .on_click(move |_, window, cx| {
                                let target = target.clone();
                                let _ = this.update(cx, |this, cx| {
                                    if this.vault.as_ref().map(|v| v.root()) != Some(target.as_path()) {
                                        this.open_vault(target, window, cx);
                                    }
                                });
                            }),
                    );
                }
                menu.separator()
                    .item(
                        PopupMenuItem::new("New vault…")
                            .icon(IconName::FolderPlus)
                            .on_click(action(|this, window, cx| this.prompt_new_vault(window, cx))),
                    )
                    .item(
                        PopupMenuItem::new("Open folder as vault…")
                            .icon(IconName::FolderOpen)
                            .on_click(action(|this, window, cx| this.prompt_open_vault(window, cx))),
                    )
                    .item(
                        PopupMenuItem::new("Show in file explorer")
                            .icon(IconName::ExternalLink)
                            .on_click(action(|this, _, cx| {
                                if let Some(vault) = &this.vault {
                                    cx.open_with_system(vault.root());
                                }
                            })),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Close vault")
                            .icon(IconName::LogOut)
                            .on_click(action(|this, window, cx| this.close_vault(window, cx))),
                    )
            })
            .into_any_element()
    }

    fn render_loading(&self, path: &std::path::Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(Spinner::new().large().color(accent(CardColor::Yellow)))
            .child(div().font_semibold().child(format!("Opening {}…", vault_name(path))))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(path.display().to_string()),
            )
            .into_any_element()
    }

    /// Shown while no vault is open.
    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let legacy = if legacy_cards_path().exists() {
            load_legacy_cards().map(|cards| cards.len()).filter(|n| *n > 0)
        } else {
            None
        };

        let tile = |id: &'static str,
                    icon: IconName,
                    title: &'static str,
                    description: &'static str,
                    cx: &mut Context<Self>,
                    on_click: fn(&mut ZettelApp, &mut Window, &mut Context<ZettelApp>)| {
            let theme = cx.theme().clone();
            v_flex()
                .id(id)
                .flex_1()
                .flex_basis(px(0.))
                .min_w_0()
                .gap_2()
                .p_4()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary.mix(theme.background, 0.4))
                .cursor_pointer()
                .hover(|s| s.border_color(accent(CardColor::Yellow).opacity(0.6)).bg(theme.secondary))
                .child(
                    div()
                        .size_8()
                        .rounded(px(8.))
                        .bg(accent(CardColor::Yellow).opacity(0.15))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).small().text_color(accent(CardColor::Yellow))),
                )
                .child(div().font_semibold().child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(description))
                .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
        };

        let recents = self
            .config
            .recent_vaults
            .iter()
            .enumerate()
            .map(|(ix, path)| {
                let exists = path.is_dir();
                let open_path = path.clone();
                let forget_path = path.clone();
                let group: SharedString = format!("recent-{ix}").into();
                h_flex()
                    .id(("recent", ix))
                    .group(group.clone())
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .when(exists, |s| s.cursor_pointer().hover(|s| s.bg(theme.secondary)))
                    .when(!exists, |s| s.opacity(0.5))
                    .child(Icon::new(IconName::Vault).small().text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(vault_name(path)))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(if exists {
                                        path.display().to_string()
                                    } else {
                                        format!("{} (missing)", path.display())
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id(("forget", ix))
                            .size_6()
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .hover(|s| s.text_color(theme.foreground).bg(theme.foreground.opacity(0.1)))
                            .child(Icon::new(IconName::X).xsmall())
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("Remove from list").build(window, cx)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.forget_vault(forget_path.clone(), cx);
                                }),
                            ),
                    )
                    .when(exists, |s| {
                        s.on_click(cx.listener(move |this, _, window, cx| {
                            this.open_vault(open_path.clone(), window, cx)
                        }))
                    })
            })
            .collect::<Vec<_>>();

        v_flex()
            .id("welcome")
            .flex_1()
            .min_h_0()
            .items_center()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(560.))
                    .px_6()
                    .py_16()
                    .gap_6()
                    .child(
                        v_flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size_16()
                                    .rounded(px(16.))
                                    .bg(accent(CardColor::Yellow))
                                    .shadow_lg()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::new(IconName::NotebookPen)
                                            .size(px(30.))
                                            .text_color(gpui_kit::black()),
                                    ),
                            )
                            .child(div().text_3xl().font_semibold().child("Zettelkasten"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .text_center()
                                    .child("Your cards live in a vault — a plain folder of Markdown files that you own."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_stretch()
                            .child(tile(
                                "new-vault",
                                IconName::FolderPlus,
                                "Create new vault",
                                "Pick a name and a place for a fresh vault folder.",
                                cx,
                                |this, window, cx| this.prompt_new_vault(window, cx),
                            ))
                            .child(tile(
                                "open-vault",
                                IconName::FolderOpen,
                                "Open folder as vault",
                                "Use an existing folder, e.g. one with Markdown notes.",
                                cx,
                                |this, window, cx| this.prompt_open_vault(window, cx),
                            )),
                    )
                    .when_some(legacy, |s, count| {
                        s.child(
                            h_flex()
                                .gap_3()
                                .p_3()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.info.opacity(0.4))
                                .bg(theme.info.opacity(0.08))
                                .text_sm()
                                .child(Icon::new(IconName::Info).small().text_color(theme.info).flex_none())
                                .child(div().flex_1().min_w_0().child(format!(
                                    "Found {count} cards from the previous version. They will be imported into the first empty vault you open."
                                ))),
                        )
                    })
                    .when(!recents.is_empty(), |s| {
                        s.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .px_3()
                                        .text_xs()
                                        .font_medium()
                                        .text_color(theme.muted_foreground)
                                        .child("RECENT VAULTS"),
                                )
                                .children(recents),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Render for ZettelApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = crate::profile::Frame::start();
        let theme = cx.theme().clone();
        let content = if let Some((path, _)) = &self.loading {
            self.render_loading(path, cx)
        } else if self.vault.is_some() {
            self.render_library(window, cx)
        } else {
            self.render_welcome(cx)
        };
        if let Some(frame) = &frame {
            frame.built();
        }

        v_flex()
            .id("zettelkasten")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_new_note))
            .on_action(cx.listener(Self::on_new_todo))
            .on_action(cx.listener(Self::on_new_snippet))
            .on_action(cx.listener(Self::on_paste_snippet))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_close_editor))
            .on_action(cx.listener(Self::on_delete_card))
            .on_action(cx.listener(Self::on_toggle_pin))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(cx.listener(Self::on_minimize))
            .on_action(cx.listener(Self::on_open_vault))
            .on_mouse_move(cx.listener(Self::on_resize_move))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.finish_resize(cx)))
            .when(self.resizing.is_some(), |s| s.cursor_col_resize())
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .relative()
            .child(self.render_title_bar(cx))
            .child(content)
            .children(self.vault.is_some().then(|| self.render_query_dialog(cx)).flatten())
            .when_some(frame, |s, frame| {
                // Painted last, so this marks the end of the frame's work.
                s.child(
                    canvas(|_, _, _| (), move |_, _, _, _| frame.painted())
                        .absolute()
                        .size_0(),
                )
            })
    }
}

impl ZettelApp {
    fn render_library(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_count;
        let viewport = window.viewport_size();
        let dock = self.render_dock(cx);
        let dock_width = self.ui.dock_width;
        let main_width = f32::from(viewport.width) - self.ui.sidebar_width - dock_width;
        let compact = main_width < 680.;
        // The popup is centered over the card list; both sidebars stay usable.
        let handles = HANDLE_WIDTH * 2.;
        let modal_area = size(
            px(main_width) - handles,
            viewport.height - gpui_kit::component::TITLE_BAR_HEIGHT,
        );
        let modal = self
            .selected_card()
            .cloned()
            .map(|card| self.render_modal(&card, modal_area, cx));
        let dock_handle = self.render_resize_handle(Resizing::Dock, cx);

        h_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_sidebar(cx))
                    .child(self.render_resize_handle(Resizing::Sidebar, cx))
                    .child(
                        v_flex()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .drag_over::<ExternalPaths>(|s, _, _, cx| s.bg(cx.theme().ring.opacity(0.06)))
                            .on_drop(cx.listener(Self::drop_files))
                            .child(self.render_header(visible, compact, cx))
                            .child(self.render_list(cx))
                            .children(self.render_undo(cx))
                            .children(modal),
                    ),
            )
            .child(dock_handle)
            .child(dock)
            .into_any_element()
    }
}
