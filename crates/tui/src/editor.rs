//! The deckbuilder (§4.5): three panes — search results, the deck, and a
//! card detail / analysis pane — over the same text deck file `deck check`
//! reads. Pure state and key handling; `editor_ui` draws it and `lib` runs
//! it (standalone, or inside the game client's lobby).

use cardsearch::{Entry, Index};
use deckstats::{CardStatus, CheckReport, Decklist, Section, Stats};
use engine::{CardDb, Format};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Focus {
    Search,
    Deck,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    /// The deck was saved (to its file, if it has one); here is the text.
    Saved(String),
    Quit,
}

/// One row of the deck pane: a type-group header or a card line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Header(String, u32),
    Card { name: String, count: u32 },
}

pub struct EditorSetup {
    pub path: Option<PathBuf>,
    pub text: String,
    pub format: Format,
    pub db: Arc<CardDb>,
    pub index: Arc<Index>,
    /// The Scryfall cache, for legality in Scryfall-pool formats.
    pub known: Option<Arc<carddb::Cache>>,
    pub theme: crate::theme::Theme,
}

pub struct Editor {
    pub path: Option<PathBuf>,
    pub format: Format,
    db: Arc<CardDb>,
    index: Arc<Index>,
    known: Option<Arc<carddb::Cache>>,
    /// Main-deck entries in insertion order.
    pub main: Vec<(String, u32)>,
    /// Sideboard and commander lines, kept as they were.
    other: Vec<deckstats::Entry>,
    pub query: String,
    pub results: Vec<Entry>,
    pub results_cursor: usize,
    pub deck_cursor: usize,
    pub focus: Focus,
    pub show_stats: bool,
    pub dirty: bool,
    undo: Vec<Vec<(String, u32)>>,
    pub status: Option<String>,
    pub report: CheckReport,
    pub stats: Stats,
    pub hands: Vec<Vec<String>>,
    hand_seed: u64,
    pub confirm_quit: bool,
    pub show_help: bool,
    disk_mtime: Option<SystemTime>,
    pub disk_changed: bool,
    pub theme: crate::theme::Theme,
}

impl Editor {
    pub fn new(setup: EditorSetup) -> Result<Editor, String> {
        let list = deckstats::parse(&setup.text).map_err(|e| e.to_string())?;
        let mut main: Vec<(String, u32)> = Vec::new();
        for e in list.main() {
            let name = setup.index.get(&e.name).map(|x| x.name.clone()).unwrap_or_else(|| e.name.clone());
            match main.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                Some((_, c)) => *c += e.count,
                None => main.push((name, e.count)),
            }
        }
        let other = list.entries.iter().filter(|e| e.section != Section::Main).cloned().collect();
        let disk_mtime = setup.path.as_ref().and_then(|p| mtime(p));
        let mut ed = Editor {
            path: setup.path,
            format: setup.format,
            db: setup.db,
            index: setup.index,
            known: setup.known,
            main,
            other,
            query: String::new(),
            results: Vec::new(),
            results_cursor: 0,
            deck_cursor: 0,
            focus: Focus::Search,
            show_stats: false,
            dirty: false,
            undo: Vec::new(),
            status: None,
            report: CheckReport::default(),
            stats: Stats::default(),
            hands: Vec::new(),
            hand_seed: 1,
            confirm_quit: false,
            show_help: false,
            disk_mtime,
            disk_changed: false,
            theme: setup.theme,
        };
        ed.search();
        ed.recompute();
        Ok(ed)
    }

    // ----- derived state -----

    pub fn card_count(&self) -> u32 {
        self.main.iter().map(|(_, c)| c).sum()
    }

    fn decklist(&self) -> Decklist {
        let mut entries: Vec<deckstats::Entry> = self
            .main
            .iter()
            .enumerate()
            .map(|(i, (name, count))| deckstats::Entry {
                count: *count,
                name: name.clone(),
                set: None,
                collector_number: None,
                line: i + 1,
                section: Section::Main,
            })
            .collect();
        entries.extend(self.other.iter().cloned());
        Decklist { entries }
    }

    pub fn text(&self) -> String {
        self.decklist().to_text(&self.db)
    }

    /// Recompute legality, stats, and (if shown) sample hands.
    fn recompute(&mut self) {
        let list = self.decklist();
        let known = self.known.as_deref().map(|k| k as &dyn deckstats::KnownCards);
        self.report = deckstats::check::check(&list, &self.format, &self.db, known);
        let deck = list.resolve(&self.db).deck;
        self.stats = Stats::compute(&deck, &self.db);
        if self.show_stats {
            self.deal_hands();
        }
        let rows = self.card_rows();
        if rows > 0 {
            self.deck_cursor = self.deck_cursor.min(rows - 1);
        } else {
            self.deck_cursor = 0;
        }
    }

    fn deal_hands(&mut self) {
        let deck = self.decklist().resolve(&self.db).deck;
        self.hands = if deck.len() >= 7 {
            deckstats::sample_hands(&deck, &self.db, &self.format, 3, self.hand_seed)
        } else {
            Vec::new()
        };
    }

    /// The deck pane's rows: cards grouped by type in the canonical order.
    pub fn rows(&self) -> Vec<Row> {
        type Group = (&'static str, u8, Vec<(String, u32)>);
        let mut groups: Vec<Group> = vec![
            ("Creatures", 0, Vec::new()),
            ("Instants", 2, Vec::new()),
            ("Sorceries", 3, Vec::new()),
            ("Artifacts", 4, Vec::new()),
            ("Enchantments", 5, Vec::new()),
            ("Lands", 6, Vec::new()),
            ("Other", 9, Vec::new()),
        ];
        for (name, count) in &self.main {
            let rank = self.db.lookup(name).map(|id| type_rank(self.db.get(id))).unwrap_or(9);
            let g = groups
                .iter_mut()
                .find(|(_, r, _)| *r == rank)
                .unwrap_or_else(|| panic!("rank {rank}"));
            g.2.push((name.clone(), *count));
        }
        let mut rows = Vec::new();
        for (title, _, mut cards) in groups {
            if cards.is_empty() {
                continue;
            }
            cards.sort_by_key(|(n, _)| {
                let mv = self.db.lookup(n).map(|id| self.db.get(id).cost.mana_value()).unwrap_or(0);
                (mv, n.to_lowercase())
            });
            rows.push(Row::Header(title.to_string(), cards.iter().map(|(_, c)| c).sum()));
            for (name, count) in cards {
                rows.push(Row::Card { name, count });
            }
        }
        rows
    }

    fn card_rows(&self) -> usize {
        self.rows().iter().filter(|r| matches!(r, Row::Card { .. })).count()
    }

    /// The card under the deck cursor.
    pub fn selected_deck_card(&self) -> Option<String> {
        self.rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Card { name, .. } => Some(name),
                _ => None,
            })
            .nth(self.deck_cursor)
    }

    pub fn selected_result(&self) -> Option<&Entry> {
        self.results.get(self.results_cursor)
    }

    /// The card whose detail is shown: the focused pane's selection.
    pub fn detail_card(&self) -> Option<Entry> {
        match self.focus {
            Focus::Search => self.selected_result().cloned(),
            Focus::Deck => self.selected_deck_card().and_then(|n| self.index.get(&n).cloned()),
        }
    }

    pub fn card_cost(&self, name: &str) -> String {
        self.db
            .lookup(name)
            .map(|id| {
                let c = self.db.get(id);
                if c.is_land() {
                    String::new()
                } else {
                    c.cost.to_string()
                }
            })
            .unwrap_or_default()
    }

    pub fn status_of(&self, name: &str) -> Option<&CardStatus> {
        self.report
            .lines
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(name))
            .map(|l| &l.status)
    }

    pub fn legality_line(&self) -> String {
        if self.report.is_legal() {
            format!("{} cards · legal in {}", self.card_count(), self.format.name)
        } else {
            let mut problems: Vec<String> = self.report.deck.iter().map(ToString::to_string).collect();
            for l in self.report.lines.iter().filter(|l| l.status != CardStatus::Ok) {
                problems.push(format!("{}: {}", l.name, l.status));
            }
            format!("{} cards · {}", self.card_count(), problems.join("; "))
        }
    }

    // ----- search -----

    fn search(&mut self) {
        let q = if self.query.trim().is_empty() {
            "is:implemented".to_string()
        } else {
            self.query.clone()
        };
        match self.index.query(&q, 0) {
            Ok(hits) => {
                // The editor only offers cards the engine can play.
                self.results = hits.into_iter().filter(|e| e.implemented).cloned().collect();
                self.status = None;
            }
            Err(e) => {
                self.results.clear();
                self.status = Some(format!("search: {e}"));
            }
        }
        self.results_cursor = self.results_cursor.min(self.results.len().saturating_sub(1));
    }

    // ----- edits -----

    fn snapshot(&mut self) {
        self.undo.push(self.main.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }

    pub fn add(&mut self, name: &str, n: u32) {
        self.snapshot();
        match self.main.iter_mut().find(|(x, _)| x.eq_ignore_ascii_case(name)) {
            Some((_, c)) => *c += n,
            None => self.main.push((name.to_string(), n)),
        }
        self.dirty = true;
        self.status = Some(format!("+{n} {name}"));
        self.recompute();
    }

    pub fn remove(&mut self, name: &str, n: u32) {
        let Some(pos) = self.main.iter().position(|(x, _)| x.eq_ignore_ascii_case(name)) else {
            return;
        };
        self.snapshot();
        let count = &mut self.main[pos].1;
        if *count > n {
            *count -= n;
            self.status = Some(format!("-{n} {name}"));
        } else {
            self.main.remove(pos);
            self.status = Some(format!("removed {name}"));
        }
        self.dirty = true;
        self.recompute();
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.main = prev;
            self.dirty = true;
            self.status = Some("undone".into());
            self.recompute();
        } else {
            self.status = Some("nothing to undo".into());
        }
    }

    /// Write the file (if there is one) and hand the text back.
    pub fn save(&mut self) -> Result<String, String> {
        let text = self.text();
        if let Some(p) = &self.path {
            // A deck saved by name may be the first thing in the user's decks directory.
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            }
            std::fs::write(p, &text).map_err(|e| format!("could not write {}: {e}", p.display()))?;
            self.disk_mtime = mtime(p);
            self.disk_changed = false;
        }
        self.dirty = false;
        self.status = Some(match &self.path {
            Some(p) => format!("saved {}", p.display()),
            None => "deck saved".into(),
        });
        Ok(text)
    }

    /// Re-read the file, discarding edits.
    pub fn reload(&mut self) {
        let Some(p) = self.path.clone() else { return };
        match std::fs::read_to_string(&p) {
            Ok(text) => match deckstats::parse(&text) {
                Ok(list) => {
                    self.main.clear();
                    for e in list.main() {
                        let name = self.index.get(&e.name).map(|x| x.name.clone()).unwrap_or_else(|| e.name.clone());
                        match self.main.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                            Some((_, c)) => *c += e.count,
                            None => self.main.push((name, e.count)),
                        }
                    }
                    self.other = list.entries.iter().filter(|e| e.section != Section::Main).cloned().collect();
                    self.dirty = false;
                    self.disk_changed = false;
                    self.disk_mtime = mtime(&p);
                    self.undo.clear();
                    self.status = Some(format!("reloaded {}", p.display()));
                    self.recompute();
                }
                Err(e) => self.status = Some(format!("could not parse {}: {e}", p.display())),
            },
            Err(e) => self.status = Some(format!("could not read {}: {e}", p.display())),
        }
    }

    /// Called on a timer: notice external edits to the file.
    pub fn check_disk(&mut self) {
        let Some(p) = &self.path else { return };
        let now = mtime(p);
        if now != self.disk_mtime && now.is_some() {
            if self.dirty {
                self.disk_changed = true;
                self.status = Some("the file changed on disk; [r] reloads it and discards your edits".into());
                self.disk_mtime = now;
            } else {
                self.reload();
            }
        }
    }

    // ----- keys -----

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<EditorCommand> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            return vec![EditorCommand::Quit];
        }
        if self.show_help {
            self.show_help = false;
            return Vec::new();
        }
        if self.confirm_quit {
            return match key.code {
                KeyCode::Char('s') | KeyCode::Char('S') => match self.save() {
                    Ok(text) => vec![EditorCommand::Saved(text), EditorCommand::Quit],
                    Err(e) => {
                        self.status = Some(e);
                        self.confirm_quit = false;
                        Vec::new()
                    }
                },
                KeyCode::Char('q') | KeyCode::Char('Q') => vec![EditorCommand::Quit],
                _ => {
                    self.confirm_quit = false;
                    Vec::new()
                }
            };
        }
        // Keys that work in either pane.
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Search => Focus::Deck,
                    Focus::Deck => Focus::Search,
                };
                return Vec::new();
            }
            KeyCode::Char('s') if ctrl => return self.try_save(),
            KeyCode::Char('z') if ctrl => {
                self.undo();
                return Vec::new();
            }
            KeyCode::Char('t') if ctrl => {
                self.toggle_stats();
                return Vec::new();
            }
            KeyCode::Char('q') if ctrl => return self.try_quit(),
            KeyCode::Char('h') if ctrl => {
                self.new_hands();
                return Vec::new();
            }
            KeyCode::Char('r') if ctrl => {
                self.reload();
                return Vec::new();
            }
            KeyCode::F(1) => {
                self.show_help = true;
                return Vec::new();
            }
            _ => {}
        }
        match self.focus {
            Focus::Search => self.key_search(key),
            Focus::Deck => self.key_deck(key),
        }
    }

    fn try_save(&mut self) -> Vec<EditorCommand> {
        match self.save() {
            Ok(text) => vec![EditorCommand::Saved(text)],
            Err(e) => {
                self.status = Some(e);
                Vec::new()
            }
        }
    }

    fn try_quit(&mut self) -> Vec<EditorCommand> {
        if self.dirty {
            self.confirm_quit = true;
            Vec::new()
        } else {
            vec![EditorCommand::Quit]
        }
    }

    fn toggle_stats(&mut self) {
        self.show_stats = !self.show_stats;
        if self.show_stats {
            self.deal_hands();
        }
    }

    fn new_hands(&mut self) {
        self.hand_seed += 1;
        self.show_stats = true;
        self.deal_hands();
    }

    fn key_search(&mut self, key: KeyEvent) -> Vec<EditorCommand> {
        match key.code {
            KeyCode::Esc => {
                if self.query.is_empty() {
                    self.focus = Focus::Deck;
                } else {
                    self.query.clear();
                    self.search();
                }
            }
            KeyCode::Up => self.results_cursor = self.results_cursor.saturating_sub(1),
            KeyCode::Down => self.results_cursor = (self.results_cursor + 1).min(self.results.len().saturating_sub(1)),
            KeyCode::PageUp => self.results_cursor = self.results_cursor.saturating_sub(10),
            KeyCode::PageDown => self.results_cursor = (self.results_cursor + 10).min(self.results.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(e) = self.selected_result().cloned() {
                    self.add(&e.name, 1);
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.search();
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                self.search();
            }
            _ => {}
        }
        Vec::new()
    }

    fn key_deck(&mut self, key: KeyEvent) -> Vec<EditorCommand> {
        let n = self.card_rows();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.deck_cursor = self.deck_cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.deck_cursor = (self.deck_cursor + 1).min(n.saturating_sub(1)),
            KeyCode::Char('/') => self.focus = Focus::Search,
            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('a') | KeyCode::Enter => {
                if let Some(name) = self.selected_deck_card() {
                    self.add(&name, 1);
                }
            }
            KeyCode::Char('-') | KeyCode::Char('d') | KeyCode::Backspace => {
                if let Some(name) = self.selected_deck_card() {
                    self.remove(&name, 1);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(name) = self.selected_deck_card() {
                    self.remove(&name, u32::MAX);
                }
            }
            KeyCode::Char('s') => return self.try_save(),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('t') => self.toggle_stats(),
            KeyCode::Char('h') => self.new_hands(),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') | KeyCode::Esc => return self.try_quit(),
            _ => {}
        }
        Vec::new()
    }

    pub fn footer(&self) -> String {
        if self.confirm_quit {
            return "Unsaved changes: [s] save and quit  [q] quit without saving  [Esc] keep editing".into();
        }
        match self.focus {
            Focus::Search => "type to search  [↑↓] move  [Enter] add  [Tab] deck pane  [^S] save  [^T] stats  [^Q] quit  [F1] help".into(),
            Focus::Deck => {
                "[↑↓] move  [+/-] count  [x] remove  [/] search  [s] save  [u] undo  [t] stats  [h] new hands  [q] quit  [?] help".into()
            }
        }
    }
}

fn type_rank(c: &engine::CardDef) -> u8 {
    use engine::CardType::*;
    if c.is_land() {
        return 6;
    }
    if c.is_creature() {
        return 0;
    }
    if c.types.contains(&Instant) {
        return 2;
    }
    if c.types.contains(&Sorcery) {
        return 3;
    }
    if c.types.contains(&Artifact) {
        return 4;
    }
    if c.types.contains(&Enchantment) {
        return 5;
    }
    9
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}
