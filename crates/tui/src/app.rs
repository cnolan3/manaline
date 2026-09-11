//! Client state and key handling, kept free of terminal I/O so it can be
//! driven and rendered in tests. Keys produce `Command`s; the runtime
//! executes them against the daemon.

use crate::settings::Settings;
use engine::text::describe_event_view;
use engine::{ActReason, Action, AttackTarget, DamageTarget, EventBase, EventView, GameView, Keyword, ObjectId, Outcome, Seat, Target};
use protocol::{LegalAction, LobbyView, ServerMessage};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// How long another seat may hold the game before the footer suggests a nudge.
pub const NUDGE_AFTER_SECS: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Act(Action),
    Chat(String),
    Refresh,
    Quit,
    /// Resubmit a deck edited in the lobby.
    SetDeck(String),
}

/// A replay being stepped through: one spectator view per action.
#[derive(Clone, Debug)]
pub struct ReplayState {
    pub title: String,
    pub views: Vec<GameView>,
    /// The events each action produced, as the spectator sees them.
    pub events: Vec<Vec<EventView>>,
    pub index: usize,
    pub playing: bool,
}

/// The deck this seat was started with, so the lobby can open the deckbuilder on it.
#[derive(Clone, Debug)]
pub struct DeckSource {
    pub path: Option<std::path::PathBuf>,
    pub text: String,
    pub format: engine::Format,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogKind {
    Game,
    Chat,
    System,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub turn: u32,
    pub kind: LogKind,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    pub action: Option<Action>,
    pub inspect: Option<ObjectId>,
}

#[derive(Clone, Debug)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct AttackPicker {
    pub candidates: Vec<ObjectId>,
    pub targets: Vec<Seat>,
    /// Per candidate: index into `targets`, or `None` for "does not attack".
    pub choice: Vec<Option<usize>>,
    pub cursor: usize,
}

#[derive(Clone, Debug)]
pub struct BlockPicker {
    pub blockers: Vec<ObjectId>,
    pub attackers: Vec<ObjectId>,
    /// Per blocker: index into `attackers`, or `None` for "does not block".
    pub choice: Vec<Option<usize>>,
    pub cursor: usize,
}

/// Pick between `min` and `count` of `items` (bottoming after a mulligan,
/// discarding to hand size, or answering an effect's or a spell's choice of
/// targets). Items are `Target`s: objects, or whole players.
#[derive(Clone, Debug)]
pub struct TargetPicker {
    pub title: String,
    pub items: Vec<Target>,
    pub marked: Vec<bool>,
    pub min: usize,
    pub count: usize,
    pub cursor: usize,
    pub reason: ActReason,
    /// Answer with `ChooseTargets` (an effect's or a cast's choice) rather than
    /// a cleanup or mulligan action.
    pub choose: bool,
    /// When answering `ChooseTargets`: every legal answer, so Enter can check
    /// the marked set against them and send the engine's own ordering.
    pub answers: Vec<Vec<Target>>,
}

impl TargetPicker {
    pub fn picked(&self) -> Vec<Target> {
        self.items.iter().zip(&self.marked).filter(|(_, m)| **m).map(|(t, _)| *t).collect()
    }

    /// The legal answer naming exactly `picked`, order-insensitively.
    fn matching_answer(&self, picked: &[Target]) -> Option<Vec<Target>> {
        let mut want = picked.to_vec();
        want.sort();
        self.answers.iter().find(|a| sorted(a) == want).cloned()
    }
}

fn sorted(targets: &[Target]) -> Vec<Target> {
    let mut v = targets.to_vec();
    v.sort();
    v
}

#[derive(Clone, Debug)]
pub struct DamagePicker {
    pub attacker: ObjectId,
    pub blockers: Vec<ObjectId>,
    pub amounts: Vec<i32>,
    pub power: i32,
    pub cursor: usize,
}

#[derive(Clone, Debug)]
pub enum Mode {
    Normal,
    Menu(Menu),
    Attack(AttackPicker),
    Block(BlockPicker),
    Damage(DamagePicker),
    Pick(TargetPicker),
    Chat(String),
    Inspect(ObjectId),
    Help,
    ConfirmConcede,
    Settings {
        selected: usize,
    },
    /// Browsing a player's graveyard, newest card first.
    Graveyard {
        seat: Seat,
        cursor: usize,
    },
}

pub const SETTINGS_ITEMS: usize = 5;

pub struct App {
    pub me: Option<Seat>,
    pub game_id: String,
    pub format_name: String,
    pub lobby: LobbyView,
    pub view: Option<GameView>,
    pub legal: Vec<LegalAction>,
    pub legal_version: u64,
    pub reason: Option<ActReason>,
    pub names: HashMap<ObjectId, String>,
    pub log: Vec<LogLine>,
    /// Lines scrolled up from the bottom of the log.
    pub log_scroll: usize,
    pub mode: Mode,
    pub status: Option<(String, Instant)>,
    pub waiting_since: Option<Instant>,
    pub expanded_opponent: usize,
    pub needs_refresh: bool,
    pub quit: bool,
    /// Text shown in the log at startup (how to connect an agent, a join command).
    pub hints: Vec<String>,
    /// Which state version last auto-opened an overlay, so it happens once per decision.
    auto_opened_at: Option<u64>,
    pub verbose_log: bool,
    /// Side panes, off by default (`l` and `s`).
    pub show_log: bool,
    pub show_stack: bool,
    pub settings: Settings,
    pub deck_source: Option<DeckSource>,
    pub replay: Option<ReplayState>,
    /// The deckbuilder, when open from the lobby.
    pub editor: Option<Box<crate::editor::Editor>>,
    /// When an armed auto-pass fires, if the current priority moment is minor.
    pub auto_pass_at: Option<Instant>,
    /// The state version the auto-pass was last armed (or held) at, so it arms once per moment.
    auto_pass_version: Option<u64>,
}

impl App {
    pub fn new(me: Option<Seat>, game_id: String, format_name: String, lobby: LobbyView) -> App {
        App {
            me,
            game_id,
            format_name,
            lobby,
            view: None,
            legal: Vec::new(),
            legal_version: 0,
            reason: None,
            names: HashMap::new(),
            log: Vec::new(),
            log_scroll: 0,
            mode: Mode::Normal,
            status: None,
            waiting_since: None,
            expanded_opponent: 0,
            needs_refresh: false,
            quit: false,
            hints: Vec::new(),
            auto_opened_at: None,
            verbose_log: false,
            show_log: false,
            show_stack: false,
            settings: Settings::default(),
            deck_source: None,
            replay: None,
            editor: None,
            auto_pass_at: None,
            auto_pass_version: None,
        }
    }

    pub fn with_settings(mut self, settings: Settings) -> App {
        self.verbose_log = settings.verbose_log;
        self.settings = settings;
        self.auto_pass_version = None;
        self.arm_auto_pass();
        self
    }

    /// A priority moment where passing is the only choice, outside your own
    /// main phases: the kind the countdown handles.
    pub fn priority_is_minor(&self) -> bool {
        let (Some(me), Some(view)) = (self.me, &self.view) else {
            return false;
        };
        if self.my_reason() != Some(ActReason::Priority) || view.outcome.is_some() {
            return false;
        }
        if view.phase.is_main() && view.active_player == me {
            return false;
        }
        self.legal
            .iter()
            .all(|l| matches!(l.action, Action::PassPriority | Action::Concede))
    }

    fn arm_auto_pass(&mut self) {
        if !self.settings.auto_pass || !self.priority_is_minor() {
            self.auto_pass_at = None;
            return;
        }
        if self.auto_pass_version == Some(self.legal_version) {
            return; // already armed or held for this moment
        }
        self.auto_pass_version = Some(self.legal_version);
        self.auto_pass_at = Some(Instant::now() + std::time::Duration::from_millis(self.settings.auto_pass_ms));
    }

    /// Cancel the countdown for the current moment.
    pub fn hold(&mut self) {
        self.auto_pass_at = None;
        self.auto_pass_version = Some(self.legal_version);
    }

    pub fn auto_pass_due(&self) -> bool {
        self.auto_pass_at.map(|t| Instant::now() >= t).unwrap_or(false) && self.priority_is_minor()
    }

    /// Seconds left on the countdown, if one is running.
    pub fn auto_pass_remaining(&self) -> Option<f64> {
        let at = self.auto_pass_at?;
        Some(at.saturating_duration_since(Instant::now()).as_secs_f64())
    }

    fn settings_changed(&mut self) {
        self.verbose_log = self.settings.verbose_log;
        if let Err(e) = self.settings.save() {
            self.set_status(format!("could not save settings: {e}"));
        }
        self.auto_pass_version = None;
        self.arm_auto_pass();
    }

    pub fn is_spectator(&self) -> bool {
        self.me.is_none()
    }

    pub fn turn(&self) -> u32 {
        self.view.as_ref().map(|v| v.turn).unwrap_or(0)
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.view.as_ref().and_then(|v| v.outcome)
    }

    pub fn name_of(&self, id: ObjectId) -> String {
        self.names.get(&id).cloned().unwrap_or_else(|| id.to_string())
    }

    pub fn object_label(&self, id: ObjectId) -> String {
        format!("{} {id}", self.name_of(id))
    }

    /// A target's name: a card name, or a seat's player name.
    pub fn target_name(&self, target: Target) -> String {
        match target {
            Target::Object(id) => self.name_of(id),
            Target::Player(seat) => self.seat_name(seat),
        }
    }

    pub fn seat_name(&self, seat: Seat) -> String {
        if let Some(v) = &self.view {
            if let Some(p) = v.players.get(seat.index()) {
                return p.name.clone();
            }
        }
        self.lobby
            .seats
            .get(seat.index())
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| format!("Seat {}", seat.0))
    }

    /// Seats other than mine, in seat order, eliminated ones included.
    pub fn opponents(&self) -> Vec<Seat> {
        let n = self.view.as_ref().map(|v| v.players.len()).unwrap_or(self.lobby.seats.len());
        (0..n).map(|i| Seat(i as u8)).filter(|s| Some(*s) != self.me).collect()
    }

    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    pub fn push_log(&mut self, kind: LogKind, text: String) {
        let turn = self.turn();
        self.log.push(LogLine { turn, kind, text });
    }

    /// Whether it is my turn to act, and why.
    pub fn my_reason(&self) -> Option<ActReason> {
        let me = self.me?;
        self.view.as_ref()?.must_act.get(&me).copied()
    }

    // ----- updates from the network -----

    pub fn set_view(&mut self, view: GameView) {
        for (id, o) in &view.objects {
            self.names.insert(*id, o.name.clone());
        }
        let was_mine = self.my_reason().is_some();
        self.view = Some(view);
        let mine = self.my_reason().is_some();
        if mine {
            self.waiting_since = None;
        } else if !was_mine || self.waiting_since.is_none() {
            self.waiting_since = Some(Instant::now());
        }
        if self.outcome().is_some() {
            self.waiting_since = None;
            if !matches!(self.mode, Mode::Normal | Mode::Help | Mode::Inspect(_)) {
                self.mode = Mode::Normal;
            }
        }
    }

    pub fn set_legal(&mut self, legal: Vec<LegalAction>, version: u64, reason: Option<ActReason>) {
        self.legal = legal;
        self.legal_version = version;
        self.reason = reason;
        self.maybe_auto_open();
        self.arm_auto_pass();
    }

    /// A pushed message from the daemon.
    pub fn handle_push(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::Event { event, state_version } => {
                if let EventBase::TurnStarted { turn, .. } = &event {
                    // Log lines after this belong to the new turn.
                    if let Some(v) = self.view.as_mut() {
                        v.turn = *turn;
                    }
                }
                self.log_event(&event);
                let current = self.view.as_ref().map(|v| v.state_version).unwrap_or(0);
                if state_version > current {
                    self.needs_refresh = true;
                }
            }
            ServerMessage::Lobby { lobby } => {
                let started = lobby.started && !self.lobby.started;
                self.lobby = lobby;
                if started {
                    self.needs_refresh = true;
                }
            }
            ServerMessage::MustAct { .. } => self.needs_refresh = true,
            _ => {}
        }
    }

    fn log_event(&mut self, event: &EventView) {
        let noisy = matches!(
            event,
            EventBase::PriorityPassed { .. }
                | EventBase::Tapped { .. }
                | EventBase::Untapped { .. }
                | EventBase::Shuffled { .. }
                | EventBase::PhaseChanged { .. }
                | EventBase::ManaAdded { .. }
        );
        if noisy && !self.verbose_log {
            return;
        }
        let kind = match event {
            EventBase::Chat { .. } => LogKind::Chat,
            _ => LogKind::Game,
        };
        let text = match event {
            EventBase::Chat { from, to, text } => {
                let who = self.seat_name(*from);
                match to {
                    Some(t) => format!("{who} → {}: {text}", self.seat_name(*t)),
                    None => format!("{who}: {text}"),
                }
            }
            e => {
                let names = |id: ObjectId| self.object_label(id);
                let seats = |s: Seat| self.seat_name(s);
                describe_event_view(e, &names, &seats)
            }
        };
        self.push_log(kind, text);
    }

    /// Open the overlay for a decision the first time it appears.
    fn maybe_auto_open(&mut self) {
        let Some(reason) = self.reason else { return };
        if !matches!(self.mode, Mode::Normal) || self.auto_opened_at == Some(self.legal_version) {
            return;
        }
        let opened = match reason {
            ActReason::Mulligan => self.open_menu("Mulligan"),
            ActReason::BottomCards => self.open_card_picker(ActReason::BottomCards),
            ActReason::Discard => self.open_card_picker(ActReason::Discard),
            // A multi-target choice is a checkbox picker; anything else is a menu.
            ActReason::Choice => self.open_target_picker(ActReason::Choice) || self.open_menu("Choose"),
            ActReason::DeclareAttackers => self.open_attack(),
            ActReason::DeclareBlockers => self.open_block(),
            ActReason::AssignDamage => self.open_damage(),
            ActReason::Priority => false,
        };
        if opened {
            self.auto_opened_at = Some(self.legal_version);
        }
    }

    // ----- overlays -----

    fn open_menu(&mut self, title: &str) -> bool {
        let items: Vec<MenuItem> = self
            .legal
            .iter()
            .filter(|l| !matches!(l.action, Action::Concede))
            .map(|l| MenuItem {
                label: l.description.clone(),
                action: Some(l.action.clone()),
                inspect: None,
            })
            .collect();
        if items.is_empty() {
            return false;
        }
        self.mode = Mode::Menu(Menu {
            title: title.into(),
            items,
            selected: 0,
        });
        true
    }

    /// A checkbox list over my whole hand for a mulligan bottom or a cleanup
    /// discard (exactly the listed count). An effect that asks the question
    /// with `ChooseTargets` instead falls through to `open_target_picker`.
    fn open_card_picker(&mut self, reason: ActReason) -> bool {
        let (Some(me), Some(view)) = (self.me, &self.view) else {
            return false;
        };
        let Some(count) = self.legal.iter().find_map(|l| match &l.action {
            Action::BottomCards { objects } | Action::Discard { objects } => Some(objects.len()),
            _ => None,
        }) else {
            return self.open_target_picker(reason);
        };
        let engine::HandView::Yours(hand) = &view.player(me).hand else {
            return false;
        };
        let items: Vec<Target> = hand.iter().copied().map(Target::Object).collect();
        if items.is_empty() || count == 0 {
            return false;
        }
        let title = match reason {
            ActReason::BottomCards => format!("Put {count} on the bottom of your library"),
            _ => format!("Discard {count} down to hand size"),
        };
        self.mode = Mode::Pick(TargetPicker {
            title,
            marked: vec![false; items.len()],
            items,
            min: count,
            count,
            cursor: 0,
            reason,
            choose: false,
            answers: Vec::new(),
        });
        true
    }

    /// A checkbox list over the targets the listed `ChooseTargets` answers
    /// name — objects or whole players — between the smallest and largest
    /// answer size. Declines (so the caller can fall back to a menu) when the
    /// pending choice isn't answered with `ChooseTargets`, when another kind of
    /// answer is legal too (`ChooseMode`: modes, "you may", pay questions), or
    /// when a plain `Choice` picks at most one target, which reads better as a
    /// menu of named answers.
    fn open_target_picker(&mut self, reason: ActReason) -> bool {
        let mut answers: Vec<Vec<Target>> = Vec::new();
        let mut items: Vec<Target> = Vec::new();
        for l in &self.legal {
            match &l.action {
                Action::ChooseTargets { targets } => {
                    for t in targets {
                        if !items.contains(t) {
                            items.push(*t);
                        }
                    }
                    answers.push(targets.clone());
                }
                Action::Concede => {}
                // Anything else in the list is not a target choice.
                _ => return false,
            }
        }
        let (Some(min), Some(count)) = (answers.iter().map(|a| a.len()).min(), answers.iter().map(|a| a.len()).max()) else {
            return false;
        };
        if count == 0 || items.is_empty() {
            return false;
        }
        if reason == ActReason::Choice && count <= 1 && min == count {
            return false;
        }
        // Objects first, in id order, then players in seat order.
        items.sort();
        // `GameView` carries no prompt for the pending choice, so the bounds
        // make the title.
        let title = match reason {
            ActReason::Discard => format!("Discard {count}"),
            _ if min == 0 => format!("Choose up to {count}"),
            _ if min == count => format!("Choose {count}"),
            _ => format!("Choose {min} to {count}"),
        };
        self.mode = Mode::Pick(TargetPicker {
            title,
            marked: vec![false; items.len()],
            items,
            min,
            count,
            cursor: 0,
            reason,
            choose: true,
            answers,
        });
        true
    }

    fn open_attack(&mut self) -> bool {
        let (Some(me), Some(view)) = (self.me, &self.view) else {
            return false;
        };
        let mut candidates: Vec<ObjectId> = view
            .player(me)
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                view.object(*id)
                    .map(|o| {
                        o.pt.is_some()
                            && !o.tapped
                            && (!o.summoning_sick || o.keywords.contains(&Keyword::Haste))
                            && !o.keywords.contains(&Keyword::Defender)
                    })
                    .unwrap_or(false)
            })
            .collect();
        candidates.sort();
        let targets: Vec<Seat> = view
            .players
            .iter()
            .filter(|p| !p.eliminated && p.seat != me)
            .map(|p| p.seat)
            .collect();
        if candidates.is_empty() || targets.is_empty() {
            return false;
        }
        let n = candidates.len();
        self.mode = Mode::Attack(AttackPicker {
            candidates,
            targets,
            choice: vec![None; n],
            cursor: 0,
        });
        true
    }

    fn open_block(&mut self) -> bool {
        let (Some(me), Some(view)) = (self.me, &self.view) else {
            return false;
        };
        let mut blockers: Vec<ObjectId> = view
            .player(me)
            .battlefield
            .iter()
            .copied()
            .filter(|id| view.object(*id).map(|o| o.pt.is_some() && !o.tapped).unwrap_or(false))
            .collect();
        blockers.sort();
        let mut attackers: Vec<ObjectId> = view
            .objects
            .values()
            .filter(|o| o.attacking == Some(AttackTarget::Player(me)))
            .map(|o| o.id)
            .collect();
        attackers.sort();
        if blockers.is_empty() || attackers.is_empty() {
            return false;
        }
        let n = blockers.len();
        self.mode = Mode::Block(BlockPicker {
            blockers,
            attackers,
            choice: vec![None; n],
            cursor: 0,
        });
        true
    }

    fn open_damage(&mut self) -> bool {
        // The first suggestion is "lethal in declared order": start from it.
        let Some(first) = self.legal.iter().find_map(|l| match &l.action {
            Action::AssignCombatDamage { attacker, assignments } => Some((*attacker, assignments.clone())),
            _ => None,
        }) else {
            return false;
        };
        let (attacker, suggested) = first;
        let Some(view) = &self.view else { return false };
        let power = view.object(attacker).and_then(|o| o.pt).map(|(p, _)| p).unwrap_or(0).max(0);
        let mut blockers: Vec<ObjectId> = view
            .objects
            .values()
            .filter(|o| o.blocking.contains(&attacker))
            .map(|o| o.id)
            .collect();
        blockers.sort();
        let amounts: Vec<i32> = blockers
            .iter()
            .map(|b| {
                suggested
                    .iter()
                    .find(|(t, _)| *t == DamageTarget::Object(*b))
                    .map(|(_, n)| *n)
                    .unwrap_or(0)
            })
            .collect();
        self.mode = Mode::Damage(DamagePicker {
            attacker,
            blockers,
            amounts,
            power,
            cursor: 0,
        });
        true
    }

    /// Client-side check of flying and reach, so the picker doesn't offer
    /// blocks the daemon would refuse.
    pub fn can_block(&self, blocker: ObjectId, attacker: ObjectId) -> bool {
        let Some(view) = &self.view else { return true };
        let (Some(b), Some(a)) = (view.object(blocker), view.object(attacker)) else {
            return true;
        };
        if a.keywords.contains(&Keyword::Flying) && !(b.keywords.contains(&Keyword::Flying) || b.keywords.contains(&Keyword::Reach)) {
            return false;
        }
        true
    }

    /// A menu of every activated ability (and equip) available right now.
    fn open_abilities_menu(&mut self) {
        let items: Vec<MenuItem> = self
            .legal
            .iter()
            .filter(|l| matches!(l.action, Action::ActivateAbility { .. }))
            .map(|l| MenuItem {
                label: l.description.clone(),
                action: Some(l.action.clone()),
                inspect: None,
            })
            .collect();
        if items.is_empty() {
            self.set_status("No abilities to activate right now");
            return;
        }
        self.mode = Mode::Menu(Menu {
            title: "Activate".into(),
            items,
            selected: 0,
        });
    }

    fn open_inspect_menu(&mut self) {
        let Some(view) = &self.view else { return };
        let mut items = Vec::new();
        let me = self.me;
        let mut ids: Vec<ObjectId> = Vec::new();
        if let Some(me) = me {
            if let engine::HandView::Yours(hand) = &view.player(me).hand {
                ids.extend(hand.iter().copied());
            }
        }
        let mut order: Vec<Seat> = Vec::new();
        if let Some(m) = me {
            order.push(m);
        }
        order.extend(self.opponents());
        for s in order {
            ids.extend(view.player(s).battlefield.iter().copied());
        }
        for so in &view.stack {
            ids.push(so.object);
        }
        for id in ids {
            if let Some(o) = view.object(id) {
                let zone = format!("{:?}", o.zone).to_lowercase();
                items.push(MenuItem {
                    label: format!("{id} {} ({zone}, {})", o.name, self.seat_name(o.controller)),
                    action: None,
                    inspect: Some(id),
                });
            }
        }
        if items.is_empty() {
            self.set_status("Nothing to inspect");
            return;
        }
        self.mode = Mode::Menu(Menu {
            title: "Inspect".into(),
            items,
            selected: 0,
        });
    }

    // ----- keys -----

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit = true;
            return vec![Command::Quit];
        }
        if let Some(ed) = self.editor.as_mut() {
            let mut out = Vec::new();
            for c in ed.handle_key(key) {
                match c {
                    crate::editor::EditorCommand::Saved(text) => out.push(Command::SetDeck(text)),
                    crate::editor::EditorCommand::Quit => self.editor = None,
                }
            }
            return out;
        }
        if self.view.is_none() && matches!(key.code, KeyCode::Char('d')) && !self.lobby.started {
            self.open_editor();
            return Vec::new();
        }
        if self.replay.is_some() && matches!(self.mode, Mode::Normal) {
            if let Some(cmds) = self.key_replay(key) {
                return cmds;
            }
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Normal => self.key_normal(key),
            Mode::Menu(menu) => self.key_menu(menu, key),
            Mode::Attack(p) => self.key_attack(p, key),
            Mode::Block(p) => self.key_block(p, key),
            Mode::Damage(p) => self.key_damage(p, key),
            Mode::Pick(p) => self.key_pick(p, key),
            Mode::Chat(text) => self.key_chat(text, key),
            Mode::Inspect(_) | Mode::Help => Vec::new(),
            Mode::Graveyard { seat, cursor } => self.key_graveyard(seat, cursor, key),
            Mode::ConfirmConcede => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => vec![Command::Act(Action::Concede)],
                _ => Vec::new(),
            },
            Mode::Settings { selected } => self.key_settings(selected, key),
        }
    }

    fn key_settings(&mut self, mut selected: usize, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('o') | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(SETTINGS_ITEMS - 1),
            KeyCode::Left | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                let dir: i64 = if key.code == KeyCode::Left { -1 } else { 1 };
                match selected {
                    0 => self.settings.auto_pass = !self.settings.auto_pass,
                    1 => {
                        let ms = self.settings.auto_pass_ms as i64 + dir * 500;
                        self.settings.auto_pass_ms = ms.clamp(500, 10_000) as u64;
                    }
                    2 => self.settings.card_keywords = !self.settings.card_keywords,
                    3 => {
                        self.settings.theme = if dir < 0 {
                            crate::theme::Theme::previous(&self.settings.theme).into()
                        } else {
                            crate::theme::Theme::next(&self.settings.theme).into()
                        }
                    }
                    _ => self.settings.verbose_log = !self.settings.verbose_log,
                }
                self.settings_changed();
            }
            _ => {}
        }
        self.mode = Mode::Settings { selected };
        Vec::new()
    }

    fn key_normal(&mut self, key: KeyEvent) -> Vec<Command> {
        let reason = self.my_reason();
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return vec![Command::Quit];
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('o') => self.mode = Mode::Settings { selected: 0 },
            KeyCode::Esc | KeyCode::Char('h') if self.auto_pass_at.is_some() => {
                self.hold();
                self.set_status("Holding priority; press Space to pass");
            }
            KeyCode::Char('c') => self.mode = Mode::Chat(String::new()),
            KeyCode::Char('i') => self.open_inspect_menu(),
            KeyCode::Char('g') => self.open_graveyard(None),
            KeyCode::Char('v') => {
                self.verbose_log = !self.verbose_log;
                self.set_status(if self.verbose_log { "Verbose log on" } else { "Verbose log off" });
            }
            KeyCode::Char('l') => self.show_log = !self.show_log,
            KeyCode::Char('s') => self.show_stack = !self.show_stack,
            KeyCode::Char('x') if self.me.is_some() && self.outcome().is_none() => self.mode = Mode::ConfirmConcede,
            KeyCode::Tab => {
                let n = self.opponents().len().max(1);
                self.expanded_opponent = (self.expanded_opponent + 1) % n;
            }
            KeyCode::PageUp => {
                self.show_log = true;
                self.log_scroll = self.log_scroll.saturating_add(5);
            }
            KeyCode::PageDown => self.log_scroll = self.log_scroll.saturating_sub(5),
            KeyCode::Char(' ') => {
                if let Some(r) = reason {
                    return self.open_for(r);
                }
            }
            KeyCode::Enter => {
                if let Some(r) = reason {
                    return self.open_for(r);
                }
                if self.me.is_some() && self.outcome().is_none() {
                    return self.nudge();
                }
            }
            KeyCode::Char('a') if reason == Some(ActReason::DeclareAttackers) => {
                self.open_attack();
            }
            KeyCode::Char('b') if reason == Some(ActReason::DeclareBlockers) => {
                self.open_block();
            }
            KeyCode::Char('d') if reason == Some(ActReason::AssignDamage) => {
                self.open_damage();
            }
            KeyCode::Char('m') if reason == Some(ActReason::Mulligan) => {
                self.open_menu("Mulligan");
            }
            KeyCode::Char('e') if reason == Some(ActReason::Priority) => {
                self.open_abilities_menu();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() && reason == Some(ActReason::Priority) => {
                let idx = if ch == '0' { 9 } else { ch as usize - '1' as usize };
                return self.play_hand_index(idx);
            }
            _ => {}
        }
        Vec::new()
    }

    fn open_for(&mut self, reason: ActReason) -> Vec<Command> {
        let opened = match reason {
            ActReason::Priority => return vec![Command::Act(Action::PassPriority)],
            ActReason::Mulligan => self.open_menu("Mulligan"),
            ActReason::BottomCards => self.open_card_picker(ActReason::BottomCards),
            ActReason::Discard => self.open_card_picker(ActReason::Discard),
            ActReason::Choice => self.open_target_picker(ActReason::Choice) || self.open_menu("Choose"),
            ActReason::DeclareAttackers => self.open_attack(),
            ActReason::DeclareBlockers => self.open_block(),
            ActReason::AssignDamage => self.open_damage(),
        };
        if !opened {
            // Nothing to pick from: take the only sensible default.
            if let Some(l) = self.legal.iter().find(|l| !matches!(l.action, Action::Concede)) {
                return vec![Command::Act(l.action.clone())];
            }
        }
        Vec::new()
    }

    /// Enter while waiting: tell whoever must act that the table is waiting on them.
    fn nudge(&mut self) -> Vec<Command> {
        let Some(view) = &self.view else {
            return Vec::new();
        };
        let mut cmds = Vec::new();
        for (seat, reason) in &view.must_act {
            let text = format!(
                "[system] {}, the table is waiting on you to {}.",
                self.seat_name(*seat),
                reason_verb(*reason)
            );
            cmds.push(Command::Chat(text));
        }
        cmds
    }

    fn play_hand_index(&mut self, idx: usize) -> Vec<Command> {
        let (Some(me), Some(view)) = (self.me, &self.view) else {
            return Vec::new();
        };
        let engine::HandView::Yours(hand) = &view.player(me).hand else {
            return Vec::new();
        };
        let Some(&id) = hand.get(idx) else {
            return Vec::new();
        };
        let options: Vec<&LegalAction> = self
            .legal
            .iter()
            .filter(|l| matches!(&l.action, Action::PlayLand { object } | Action::CastSpell { object, .. } if *object == id))
            .collect();
        match options.len() {
            0 => {
                let name = self.name_of(id);
                self.set_status(format!("{name} can't be played right now"));
                Vec::new()
            }
            1 => vec![Command::Act(options[0].action.clone())],
            _ => {
                let items = options
                    .iter()
                    .map(|l| MenuItem {
                        label: l.description.clone(),
                        action: Some(l.action.clone()),
                        inspect: None,
                    })
                    .collect();
                self.mode = Mode::Menu(Menu {
                    title: format!("Pay for {}", self.name_of(id)),
                    items,
                    selected: 0,
                });
                Vec::new()
            }
        }
    }

    fn key_menu(&mut self, mut menu: Menu, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1)),
            KeyCode::PageUp => menu.selected = menu.selected.saturating_sub(10),
            KeyCode::PageDown => menu.selected = (menu.selected + 10).min(menu.items.len().saturating_sub(1)),
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                let idx = if ch == '0' { 9 } else { ch as usize - '1' as usize };
                if idx < menu.items.len() {
                    menu.selected = idx;
                    return self.select_menu_item(menu);
                }
            }
            KeyCode::Enter => return self.select_menu_item(menu),
            _ => {}
        }
        self.mode = Mode::Menu(menu);
        Vec::new()
    }

    fn select_menu_item(&mut self, menu: Menu) -> Vec<Command> {
        let Some(item) = menu.items.get(menu.selected) else {
            return Vec::new();
        };
        if let Some(id) = item.inspect {
            self.mode = Mode::Inspect(id);
            return Vec::new();
        }
        match &item.action {
            Some(a) => vec![Command::Act(a.clone())],
            None => Vec::new(),
        }
    }

    fn key_attack(&mut self, mut p: AttackPicker, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.cursor = (p.cursor + 1).min(p.candidates.len() - 1),
            KeyCode::Char(' ') => {
                p.choice[p.cursor] = match p.choice[p.cursor] {
                    None => Some(0),
                    Some(_) => None,
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Left => {
                let n = p.targets.len();
                p.choice[p.cursor] = match p.choice[p.cursor] {
                    None => Some(0),
                    Some(t) if key.code == KeyCode::Left => Some((t + n - 1) % n),
                    Some(t) => Some((t + 1) % n),
                };
            }
            KeyCode::Char('a') => {
                for c in p.choice.iter_mut() {
                    *c = Some(0);
                }
            }
            KeyCode::Char('n') => {
                for c in p.choice.iter_mut() {
                    *c = None;
                }
            }
            KeyCode::Enter => {
                let attackers = p
                    .candidates
                    .iter()
                    .zip(&p.choice)
                    .filter_map(|(id, c)| c.map(|t| (*id, AttackTarget::Player(p.targets[t]))))
                    .collect();
                return vec![Command::Act(Action::DeclareAttackers { attackers })];
            }
            _ => {}
        }
        self.mode = Mode::Attack(p);
        Vec::new()
    }

    fn key_block(&mut self, mut p: BlockPicker, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.cursor = (p.cursor + 1).min(p.blockers.len() - 1),
            KeyCode::Char(' ') => {
                p.choice[p.cursor] = match p.choice[p.cursor] {
                    None => Some(0),
                    Some(_) => None,
                };
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Left => {
                let n = p.attackers.len();
                let blocker = p.blockers[p.cursor];
                let can = |a: usize| self.can_block(blocker, p.attackers[a]);
                let mut next = match p.choice[p.cursor] {
                    None => Some(0),
                    Some(a) if key.code == KeyCode::Left => Some((a + n - 1) % n),
                    Some(a) => Some((a + 1) % n),
                };
                // Skip attackers this creature can't block (flying without reach).
                for _ in 0..n {
                    match next {
                        Some(a) if !can(a) => next = Some((a + 1) % n),
                        _ => break,
                    }
                }
                if let Some(a) = next {
                    if !can(a) {
                        next = None;
                        self.set_status("This creature can't block any attacker (flying)");
                    }
                }
                p.choice[p.cursor] = next;
            }
            KeyCode::Char('n') => {
                for c in p.choice.iter_mut() {
                    *c = None;
                }
            }
            KeyCode::Enter => {
                let blocks = p
                    .blockers
                    .iter()
                    .zip(&p.choice)
                    .filter_map(|(b, c)| c.map(|a| (*b, p.attackers[a])))
                    .collect();
                return vec![Command::Act(Action::DeclareBlockers { blocks })];
            }
            _ => {}
        }
        self.mode = Mode::Block(p);
        Vec::new()
    }

    fn key_pick(&mut self, mut p: TargetPicker, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.cursor = (p.cursor + 1).min(p.items.len() - 1),
            KeyCode::Char(' ') => {
                let marked = p.marked.iter().filter(|m| **m).count();
                if p.marked[p.cursor] {
                    p.marked[p.cursor] = false;
                } else if marked < p.count {
                    p.marked[p.cursor] = true;
                } else {
                    self.set_status(format!("Pick at most {}; unmark one first", p.count));
                }
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                let idx = if ch == '0' { 9 } else { ch as usize - '1' as usize };
                if idx < p.items.len() {
                    p.cursor = idx;
                    let marked = p.marked.iter().filter(|m| **m).count();
                    if p.marked[idx] {
                        p.marked[idx] = false;
                    } else if marked < p.count {
                        p.marked[idx] = true;
                    }
                }
            }
            KeyCode::Enter => {
                let picked = p.picked();
                if picked.len() < p.min || picked.len() > p.count {
                    let want = if p.min == p.count {
                        format!("exactly {}", p.count)
                    } else {
                        format!("{} to {}", p.min, p.count)
                    };
                    self.set_status(format!("Pick {want} ({} marked)", picked.len()));
                } else if p.choose {
                    // The marked set has to be one of the engine's answers, and
                    // is sent in the engine's own order (it groups by spec).
                    match p.matching_answer(&picked) {
                        Some(targets) => return vec![Command::Act(Action::ChooseTargets { targets })],
                        None => {
                            let names: Vec<String> = picked.iter().map(|t| self.target_name(*t)).collect();
                            self.set_status(format!("That combination isn't a legal choice: {}", names.join(", ")));
                        }
                    }
                } else {
                    let objects: Vec<ObjectId> = picked
                        .iter()
                        .filter_map(|t| match t {
                            Target::Object(id) => Some(*id),
                            Target::Player(_) => None,
                        })
                        .collect();
                    let action = match p.reason {
                        ActReason::BottomCards => Action::BottomCards { objects },
                        _ => Action::Discard { objects },
                    };
                    return vec![Command::Act(action)];
                }
            }
            _ => {}
        }
        self.mode = Mode::Pick(p);
        Vec::new()
    }

    fn key_damage(&mut self, mut p: DamagePicker, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.cursor = (p.cursor + 1).min(p.blockers.len() - 1),
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                let total: i32 = p.amounts.iter().sum();
                if total < p.power {
                    p.amounts[p.cursor] += 1;
                }
            }
            KeyCode::Left | KeyCode::Char('-') if p.amounts[p.cursor] > 0 => p.amounts[p.cursor] -= 1,
            KeyCode::Enter => {
                let total: i32 = p.amounts.iter().sum();
                if total != p.power {
                    self.set_status(format!("Assign exactly {} damage ({total} assigned)", p.power));
                    self.mode = Mode::Damage(p);
                    return Vec::new();
                }
                let assignments = p
                    .blockers
                    .iter()
                    .zip(&p.amounts)
                    .filter(|(_, n)| **n > 0)
                    .map(|(b, n)| (DamageTarget::Object(*b), *n))
                    .collect();
                return vec![Command::Act(Action::AssignCombatDamage {
                    attacker: p.attacker,
                    assignments,
                })];
            }
            _ => {}
        }
        self.mode = Mode::Damage(p);
        Vec::new()
    }

    fn key_chat(&mut self, mut text: String, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Enter => {
                let t = text.trim().to_string();
                if t.is_empty() {
                    return Vec::new();
                }
                return vec![Command::Chat(t)];
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(ch) => text.push(ch),
            _ => {}
        }
        self.mode = Mode::Chat(text);
        Vec::new()
    }

    /// Whether the footer should suggest nudging the seat that must act.
    pub fn waiting_long(&self) -> bool {
        self.waiting_since
            .map(|t| t.elapsed().as_secs() >= NUDGE_AFTER_SECS)
            .unwrap_or(false)
    }

    /// The footer text: every legal action has a key, so it is generated from state.
    /// Load a replay and show its first position.
    pub fn load_replay(&mut self, replay: ReplayState) {
        self.replay = Some(replay);
        self.replay_seek(0);
    }

    /// Jump to action `i` of the replay, rebuilding the log up to it.
    pub fn replay_seek(&mut self, i: usize) {
        let Some(r) = self.replay.as_mut() else { return };
        if r.views.is_empty() {
            return;
        }
        r.index = i.min(r.views.len() - 1);
        let index = r.index;
        let view = r.views[index].clone();
        let events: Vec<EventView> = r.events[..=index].iter().flatten().cloned().collect();
        let keep: Vec<LogLine> = self.log.iter().filter(|l| l.kind == LogKind::System).cloned().collect();
        self.log = keep;
        self.set_view(view);
        for e in events {
            self.log_event(&e);
        }
        self.log_scroll = 0;
    }

    pub fn replay_step(&mut self, delta: i64) {
        let Some(r) = &self.replay else { return };
        let i = (r.index as i64 + delta).clamp(0, r.views.len() as i64 - 1) as usize;
        self.replay_seek(i);
    }

    /// The next action index where the turn number changes, in `dir`.
    fn replay_turn_boundary(&self, dir: i64) -> Option<usize> {
        let r = self.replay.as_ref()?;
        let turn = r.views[r.index].turn;
        let mut i = r.index as i64;
        loop {
            i += dir;
            if i < 0 || i >= r.views.len() as i64 {
                return Some(if dir < 0 { 0 } else { r.views.len() - 1 });
            }
            if r.views[i as usize].turn != turn {
                // Going backwards, land on the start of the previous turn.
                if dir < 0 {
                    let t = r.views[i as usize].turn;
                    while i > 0 && r.views[i as usize - 1].turn == t {
                        i -= 1;
                    }
                }
                return Some(i as usize);
            }
        }
    }

    /// Replay controls; `None` lets the key fall through to the normal handler.
    fn key_replay(&mut self, key: KeyEvent) -> Option<Vec<Command>> {
        match key.code {
            KeyCode::Right | KeyCode::Char('n') => self.replay_step(1),
            KeyCode::Left | KeyCode::Char('p') => self.replay_step(-1),
            KeyCode::Char(' ') | KeyCode::Char('P') => {
                if let Some(r) = self.replay.as_mut() {
                    r.playing = !r.playing;
                }
            }
            KeyCode::Char(']') => {
                if let Some(i) = self.replay_turn_boundary(1) {
                    self.replay_seek(i);
                }
            }
            KeyCode::Char('[') => {
                if let Some(i) = self.replay_turn_boundary(-1) {
                    self.replay_seek(i);
                }
            }
            KeyCode::Home => self.replay_seek(0),
            KeyCode::End => self.replay_seek(usize::MAX),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// Advance one step while playing; called on a timer.
    pub fn replay_tick(&mut self) {
        let Some(r) = &self.replay else { return };
        if r.playing {
            if r.index + 1 >= r.views.len() {
                if let Some(r) = self.replay.as_mut() {
                    r.playing = false;
                }
            } else {
                self.replay_step(1);
            }
        }
    }

    /// "P1 conceded" / "P1 was reduced to 0 life", for the losers of a finished game.
    pub fn elimination_text(&self) -> String {
        let Some(view) = &self.view else { return String::new() };
        let parts: Vec<String> = view
            .players
            .iter()
            .filter_map(|p| {
                p.elimination.as_ref().map(|e| {
                    format!(
                        "{} {}",
                        if Some(p.seat) == self.me {
                            "you".to_string()
                        } else {
                            p.name.clone()
                        },
                        e.phrase()
                    )
                })
            })
            .collect();
        parts.join(", ")
    }

    /// Open the graveyard browser on `seat` (yours first, then Tab through the table).
    pub fn open_graveyard(&mut self, seat: Option<Seat>) {
        let Some(view) = &self.view else { return };
        let seat = seat.or(self.me).or_else(|| view.players.first().map(|p| p.seat));
        if let Some(seat) = seat {
            self.mode = Mode::Graveyard { seat, cursor: 0 };
        }
    }

    fn key_graveyard(&mut self, seat: Seat, mut cursor: usize, key: KeyEvent) -> Vec<Command> {
        let Some(view) = &self.view else { return Vec::new() };
        let cards: Vec<ObjectId> = view.player(seat).graveyard.iter().rev().copied().collect();
        match key.code {
            KeyCode::Esc | KeyCode::Char('g') | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(cards.len().saturating_sub(1)),
            KeyCode::Tab => {
                let seats: Vec<Seat> = view.players.iter().map(|p| p.seat).collect();
                let i = seats.iter().position(|s| *s == seat).unwrap_or(0);
                let next = seats[(i + 1) % seats.len()];
                self.mode = Mode::Graveyard { seat: next, cursor: 0 };
                return Vec::new();
            }
            KeyCode::Enter | KeyCode::Char('i') => {
                if let Some(&id) = cards.get(cursor) {
                    self.mode = Mode::Inspect(id);
                    return Vec::new();
                }
            }
            _ => {}
        }
        self.mode = Mode::Graveyard { seat, cursor };
        Vec::new()
    }

    pub fn theme(&self) -> crate::theme::Theme {
        crate::theme::Theme::named(&self.settings.theme).unwrap_or_default()
    }

    /// Open the deckbuilder on this seat's deck (lobby only).
    pub fn open_editor(&mut self) {
        let Some(src) = self.deck_source.clone() else {
            self.set_status("no deck to edit on this seat");
            return;
        };
        let db = Arc::new(cards::core());
        let known = carddb::Cache::load().ok().flatten().map(Arc::new);
        let index = Arc::new(match &known {
            Some(c) => cardsearch::Index::from_cache(c, &db),
            None => cardsearch::Index::from_db(&db),
        });
        let theme = self.theme();
        match crate::editor::Editor::new(crate::editor::EditorSetup {
            path: src.path,
            text: src.text,
            format: src.format,
            db,
            index,
            known,
            theme,
        }) {
            Ok(ed) => self.editor = Some(Box::new(ed)),
            Err(e) => self.set_status(format!("could not open the deck: {e}")),
        }
    }

    pub fn footer(&self) -> String {
        if let (Some(r), Mode::Normal) = (&self.replay, &self.mode) {
            return format!(
                "REPLAY {}/{}  [→/n] step  [←/p] back  [[/]] turn  [Space] {}  [Home/End]  [l] log  [s] stack  [i] inspect  [q] quit",
                r.index + 1,
                r.views.len(),
                if r.playing { "pause" } else { "play" }
            );
        }
        if self.outcome().is_some() {
            return "[q] quit  [l] log  [i] inspect  [g] graveyards".into();
        }
        if self.view.is_none() {
            return if self.deck_source.is_some() && !self.lobby.started {
                "Waiting for the game to start…  [d] edit your deck  [q] quit".into()
            } else {
                "Waiting for the game to start…  [q] quit".into()
            };
        }
        match &self.mode {
            Mode::Menu(_) => return "[↑↓] move  [Enter] choose  [1-9] jump  [Esc] cancel".into(),
            Mode::Attack(_) => return "[Space] toggle attack  [Tab] cycle target  [a] all  [n] none  [Enter] declare  [Esc] cancel".into(),
            Mode::Block(_) => return "[Space] toggle block  [Tab] cycle attacker  [n] none  [Enter] declare  [Esc] cancel".into(),
            Mode::Damage(_) => return "[←→/+-] adjust  [↑↓] move  [Enter] assign  [Esc] cancel".into(),
            Mode::Pick(_) => return "[Space]/[1-9] toggle  [↑↓] move  [Enter] confirm  [Esc] cancel".into(),
            Mode::Chat(_) => return "type a message  [Enter] send  [Esc] cancel".into(),
            Mode::Inspect(_) | Mode::Help => return "[Esc] close".into(),
            Mode::Graveyard { .. } => return "[↑↓] move  [Enter] card details  [Tab] next player's graveyard  [Esc] close".into(),
            Mode::ConfirmConcede => return "Concede the game? [y] yes  [any other key] no".into(),
            Mode::Settings { .. } => return "[↑↓] move  [←→/Enter] change  [Esc] close".into(),
            Mode::Normal => {}
        }
        if let Some(left) = self.auto_pass_remaining() {
            return format!("Auto-passing in {left:.1}s  [Space] pass now  [Esc] hold  [o] settings  [?] help");
        }
        match self.my_reason() {
            Some(ActReason::Priority) => {
                let mut parts = vec!["[Space] pass".to_string()];
                if self
                    .legal
                    .iter()
                    .any(|l| matches!(l.action, Action::PlayLand { .. } | Action::CastSpell { .. }))
                {
                    parts.push("[1-9] play/cast".into());
                }
                if self.legal.iter().any(|l| matches!(l.action, Action::ActivateAbility { .. })) {
                    parts.push("[e] abilities".into());
                }
                parts.extend(
                    [
                        "[i] inspect",
                        "[c] chat",
                        "[l] log",
                        "[s] stack",
                        "[o] settings",
                        "[x] concede",
                        "[?] help",
                    ]
                    .map(String::from),
                );
                parts.join("  ")
            }
            Some(reason) => {
                let key = match reason {
                    ActReason::DeclareAttackers => "[a]/[Enter] declare attackers",
                    ActReason::DeclareBlockers => "[b]/[Enter] declare blockers",
                    ActReason::AssignDamage => "[d]/[Enter] assign damage",
                    ActReason::Mulligan => "[m]/[Enter] mulligan decision",
                    _ => "[Enter] choose",
                };
                format!("{key}  [i] inspect  [c] chat  [l] log  [?] help")
            }
            None => {
                let view = self.view.as_ref().unwrap();
                let waiting: Vec<String> = view
                    .must_act
                    .iter()
                    .map(|(s, r)| format!("{} to {}", self.seat_name(*s), reason_verb(*r)))
                    .collect();
                if self.waiting_long() && !self.is_spectator() {
                    format!(
                        "Waiting on {} — nudge them with [Enter]  [c] chat  [l] log  [?] help",
                        waiting.join(", ")
                    )
                } else {
                    format!("Waiting on {}…  [i] inspect  [c] chat  [l] log  [?] help", waiting.join(", "))
                }
            }
        }
    }
}

pub fn reason_verb(r: ActReason) -> &'static str {
    match r {
        ActReason::Priority => "act",
        ActReason::DeclareAttackers => "declare attackers",
        ActReason::DeclareBlockers => "declare blockers",
        ActReason::AssignDamage => "assign combat damage",
        ActReason::Mulligan => "decide on a mulligan",
        ActReason::BottomCards => "put cards on the bottom",
        ActReason::Discard => "discard",
        ActReason::Choice => "make a choice",
    }
}
