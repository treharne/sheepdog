use crate::input::{self, Field};
use crossterm::event::{KeyCode as K, KeyEvent, KeyEventKind, KeyModifiers as M};
use sheepdog_core::{Geometry, Space, SpaceId, Tab, TabId, TerminalState};

#[derive(Clone, Debug)]
pub enum Action {
    Refresh,
    Select(TabId),
    NewTab(SpaceId),
    NewSpace(String, String),
    CloseTab(TabId),
    CloseSpace(SpaceId),
    Reopen(TabId),
}
pub enum Effect {
    None,
    Work(Action),
    Send(Vec<u8>),
    Quit,
}
#[derive(Clone)]
pub struct Choice {
    pub space: SpaceId,
    pub tab: Option<TabId>,
    pub label: String,
}
pub enum Modal {
    Help,
    Name(Field),
    Directory(String, Field),
    ConfirmTab(TabId, String),
    ConfirmSpace(SpaceId, String),
    Picker {
        spaces: bool,
        choices: Vec<Choice>,
        cursor: usize,
        filter: Field,
        search: bool,
    },
}
pub struct App {
    pub spaces: Vec<Space>,
    pub tabs: Vec<Tab>,
    pub space: Option<SpaceId>,
    pub selected: Option<TabId>,
    pub width: u16,
    pub height: u16,
    pub prefix: bool,
    pub modal: Option<Modal>,
    pub notice: String,
    pub busy: bool,
    pub literal: bool,
}
impl App {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            spaces: vec![],
            tabs: vec![],
            space: None,
            selected: None,
            width,
            height,
            prefix: false,
            modal: None,
            notice: String::new(),
            busy: false,
            literal: false,
        }
    }
    pub fn usable(&self) -> bool {
        self.width >= 12 && self.height >= 5
    }
    pub fn geometry(&self) -> Geometry {
        Geometry {
            cols: self.width.clamp(1, 500),
            rows: self.height.saturating_sub(3).clamp(1, 300),
        }
    }
    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| Some(t.id) == self.selected)
    }
    pub fn live(&self) -> bool {
        self.tab()
            .is_some_and(|t| matches!(t.terminal, TerminalState::Running(_)))
    }
    pub fn snapshot(&mut self, spaces: Vec<Space>, tabs: Vec<Tab>) {
        self.spaces = spaces;
        self.tabs = tabs;
        if !self.spaces.iter().any(|s| Some(s.id) == self.space) {
            self.space = self.spaces.first().map(|s| s.id);
            self.selected = None;
        }
        if !self
            .tabs
            .iter()
            .any(|t| Some(t.id) == self.selected && Some(t.space) == self.space)
        {
            self.selected = self
                .tabs
                .iter()
                .find(|t| Some(t.space) == self.space)
                .map(|t| t.id);
        }
    }
    pub fn choose(&mut self, id: TabId) -> Effect {
        if self.busy {
            self.notice = "Control operation pending; selection unchanged".into();
            return Effect::None;
        }
        if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
            self.space = Some(tab.space);
            self.selected = Some(id);
            self.notice.clear();
            return Effect::Work(Action::Select(id));
        }
        Effect::None
    }
    fn picker(&mut self, spaces: bool) {
        let mut choices = vec![];
        for space in &self.spaces {
            if spaces {
                choices.push(Choice {
                    space: space.id,
                    tab: None,
                    label: format!("{} — {}", space.name, space.default_directory.display()),
                });
            } else {
                for tab in self.tabs.iter().filter(|t| t.space == space.id) {
                    choices.push(Choice {
                        space: space.id,
                        tab: Some(tab.id),
                        label: format!(
                            "{} / {} #{}{}",
                            space.name,
                            tab.label,
                            tab.id.0,
                            if matches!(tab.terminal, TerminalState::Ended(_)) {
                                " [ended]"
                            } else {
                                ""
                            }
                        ),
                    });
                }
            }
        }
        let cursor = choices
            .iter()
            .position(|c| {
                if spaces {
                    Some(c.space) == self.space
                } else {
                    c.tab == self.selected
                }
            })
            .unwrap_or(0);
        self.modal = Some(Modal::Picker {
            spaces,
            choices,
            cursor,
            filter: Field::default(),
            search: false,
        });
    }
    fn prefix(&mut self, key: KeyEvent) -> Effect {
        self.prefix = false;
        if input::ctrl(key, 'b') {
            return Effect::Send(vec![2]);
        }
        let c = match key.code {
            K::Char(c) if key.modifiers.contains(M::SHIFT) => c.to_ascii_uppercase(),
            K::Char(c) => c,
            _ => '\0',
        };
        if c == 'q' {
            return Effect::Quit;
        }
        if c == '?' {
            self.modal = Some(Modal::Help);
            return Effect::None;
        }
        if c == 'P' {
            self.literal = true;
            self.notice = "Next paste is literal (File Ingest bypass)".into();
            return Effect::None;
        }
        if self.busy {
            self.notice = "Control operation pending; action not sent".into();
            return Effect::None;
        }
        match c {
            'w' => self.picker(true),
            'g' => self.picker(false),
            'N' => self.modal = Some(Modal::Name(Field::default())),
            'c' => {
                if let Some(space) = self.space {
                    return Effect::Work(Action::NewTab(space));
                }
            }
            'X' => {
                if let Some(t) = self.tab() {
                    self.modal = Some(Modal::ConfirmTab(t.id, t.label.clone()));
                }
            }
            'D' => {
                if let Some(s) = self.spaces.iter().find(|s| Some(s.id) == self.space) {
                    self.modal = Some(Modal::ConfirmSpace(s.id, s.name.clone()));
                }
            }
            'n' | 'p' | '1'..='9' => {
                let tabs: Vec<_> = self
                    .tabs
                    .iter()
                    .filter(|t| Some(t.space) == self.space)
                    .map(|t| t.id)
                    .collect();
                if !tabs.is_empty() {
                    let old = tabs
                        .iter()
                        .position(|t| Some(*t) == self.selected)
                        .unwrap_or(0);
                    let i = match c {
                        'n' => (old + 1) % tabs.len(),
                        'p' => (old + tabs.len() - 1) % tabs.len(),
                        c => c as usize - '1' as usize,
                    };
                    if let Some(id) = tabs.get(i) {
                        return self.choose(*id);
                    }
                }
            }
            _ => {
                if key.code == K::Enter
                    && let Some(t) = self.tab()
                    && matches!(t.terminal, TerminalState::Ended(_))
                {
                    return Effect::Work(Action::Reopen(t.id));
                } else if key.code != K::Esc {
                    self.notice =
                        "Binding not implemented yet; Ctrl+B ? lists supported actions".into();
                }
            }
        }
        Effect::None
    }
    pub fn key(&mut self, key: KeyEvent) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        // Naming fields own editing chords, including Ctrl+B/F, as in Herdr.
        if !matches!(self.modal, Some(Modal::Name(_) | Modal::Directory(..))) {
            if self.prefix {
                return self.prefix(key);
            }
            if input::ctrl(key, 'b') {
                self.prefix = true;
                return Effect::None;
            }
        }
        if let Some(mut modal) = self.modal.take() {
            if key.code == K::Esc {
                return Effect::None;
            }
            let mut retain = true;
            let effect = match &mut modal {
                Modal::Help => {
                    if key.code == K::Char('q') {
                        retain = false;
                    }
                    Effect::None
                }
                Modal::Name(field) => {
                    if key.code == K::Enter && !field.text.trim().is_empty() {
                        self.modal = Some(Modal::Directory(field.text.clone(), Field::default()));
                        return Effect::None;
                    } else {
                        field.key(key);
                        Effect::None
                    }
                }
                Modal::Directory(name, field) => {
                    if key.code == K::Enter {
                        if !std::path::Path::new(&field.text).is_absolute() {
                            self.notice = "Use an absolute directory on the remote machine".into();
                            Effect::None
                        } else {
                            retain = false;
                            Effect::Work(Action::NewSpace(name.clone(), field.text.clone()))
                        }
                    } else {
                        field.key(key);
                        Effect::None
                    }
                }
                Modal::ConfirmTab(id, _) => {
                    if key.code == K::Char('y') {
                        retain = false;
                        Effect::Work(Action::CloseTab(*id))
                    } else {
                        if key.code == K::Char('n') {
                            retain = false;
                        }
                        Effect::None
                    }
                }
                Modal::ConfirmSpace(id, _) => {
                    if key.code == K::Char('y') {
                        retain = false;
                        Effect::Work(Action::CloseSpace(*id))
                    } else {
                        if key.code == K::Char('n') {
                            retain = false;
                        }
                        Effect::None
                    }
                }
                Modal::Picker {
                    spaces,
                    choices,
                    cursor,
                    filter,
                    search,
                } => {
                    let visible = filtered(choices, &filter.text);
                    if key.code == K::Enter
                        && let Some(choice) = visible.get(*cursor)
                    {
                        let choice = (*choice).clone();
                        retain = false;
                        if *spaces {
                            self.space = Some(choice.space);
                            self.selected = self
                                .tabs
                                .iter()
                                .find(|t| t.space == choice.space)
                                .map(|t| t.id);
                            if let Some(id) = self.selected {
                                self.choose(id)
                            } else {
                                Effect::None
                            }
                        } else {
                            self.choose(choice.tab.unwrap())
                        }
                    } else if key.code == K::Up || (!*search && key.code == K::Char('k')) {
                        *cursor = cursor.saturating_sub(1);
                        Effect::None
                    } else if key.code == K::Down || (!*search && key.code == K::Char('j')) {
                        *cursor = (*cursor + 1).min(visible.len().saturating_sub(1));
                        Effect::None
                    } else if !*search && matches!(key.code, K::Left | K::Right) && !*spaces {
                        if let Some(current) = visible.get(*cursor) {
                            let group = current.space;
                            if key.code == K::Right {
                                if let Some(i) = visible
                                    .iter()
                                    .enumerate()
                                    .skip(*cursor + 1)
                                    .find(|(_, c)| c.space != group)
                                    .map(|(i, _)| i)
                                {
                                    *cursor = i;
                                }
                            } else if let Some(previous) =
                                visible[..*cursor].iter().rev().find(|c| c.space != group)
                            {
                                *cursor = visible
                                    .iter()
                                    .position(|c| c.space == previous.space)
                                    .unwrap();
                            }
                        }
                        Effect::None
                    } else if key.code == K::Char('/') && !*search {
                        *search = true;
                        Effect::None
                    } else if *search {
                        field_edit(filter, key, cursor);
                        Effect::None
                    } else {
                        if key.code == K::Char('q') {
                            retain = false;
                        }
                        Effect::None
                    }
                }
            };
            if retain {
                self.modal = Some(modal);
            }
            return effect;
        }
        input::remote(key).map_or(Effect::None, Effect::Send)
    }
}
fn field_edit(field: &mut Field, key: KeyEvent, cursor: &mut usize) {
    field.key(key);
    *cursor = 0;
}
pub fn filtered<'a>(choices: &'a [Choice], filter: &str) -> Vec<&'a Choice> {
    let needle = filter.to_lowercase();
    choices
        .iter()
        .filter(|c| c.label.to_lowercase().contains(&needle))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(K::Char(c), M::NONE)
    }
    fn prefix(app: &mut App, c: char) -> Effect {
        app.key(KeyEvent::new(K::Char('b'), M::CONTROL));
        app.key(key(c))
    }
    #[test]
    fn prefix_quit_remains_available_while_control_is_busy() {
        let mut app = App::new(80, 24);
        app.busy = true;
        assert!(matches!(prefix(&mut app, 'q'), Effect::Quit));
    }
    #[test]
    fn prefix_unknown_never_reaches_the_shell_and_double_prefix_does() {
        let mut app = App::new(80, 24);
        assert!(matches!(prefix(&mut app, 'v'), Effect::None));
        app.key(KeyEvent::new(K::Char('b'), M::CONTROL));
        assert!(
            matches!(app.key(KeyEvent::new(K::Char('b'),M::CONTROL)),Effect::Send(v) if v==[2])
        );
    }
    #[test]
    fn confirmation_retains_the_original_target() {
        let mut app = App::new(80, 24);
        app.modal = Some(Modal::ConfirmTab(TabId(7), "old".into()));
        app.selected = Some(TabId(9));
        assert!(matches!(
            app.key(key('y')),
            Effect::Work(Action::CloseTab(TabId(7)))
        ));
    }
    #[test]
    fn dimensions_reserve_navigation_and_footer() {
        let app = App::new(80, 24);
        assert_eq!(app.geometry(), Geometry { cols: 80, rows: 21 });
        assert_eq!(
            App::new(1000, 1000).geometry(),
            Geometry {
                cols: 500,
                rows: 300
            }
        );
        assert!(!App::new(2, 2).usable());
    }
}
