//! Shared paint/hit geometry: mouse targets never use a second layout formula.
use crate::app::App;
use sheepdog_core::{SpaceId, TabId, TerminalState};
use unicode_width::UnicodeWidthStr;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Rect {
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Space(SpaceId),
    Tab(TabId),
    NewSpace,
    NewTab,
}
pub struct Entry {
    pub rect: Rect,
    pub text: String,
    pub target: Option<Target>,
    pub active: bool,
}
pub struct Layout {
    pub body: Rect,
    pub sidebar: u16,
    pub entries: Vec<Entry>,
}
pub fn compute(app: &App) -> Layout {
    let sidebar = if app.width >= 60 {
        26
    } else if app.width >= 36 {
        18
    } else {
        0
    };
    let body = Rect {
        x: sidebar,
        y: 1,
        width: app.width.saturating_sub(sidebar),
        height: app.height.saturating_sub(1),
    };
    let mut entries = vec![];
    if sidebar > 0 && app.height >= 6 {
        let width = sidebar - 1;
        let split = (app.height / 2).max(3);
        entries.push(Entry {
            rect: Rect {
                x: 0,
                y: 0,
                width,
                height: 1,
            },
            text: "Spaces".into(),
            target: None,
            active: false,
        });
        let count = split.saturating_sub(2) as usize;
        let selected = app
            .spaces
            .iter()
            .position(|s| Some(s.id) == app.space)
            .unwrap_or(0);
        let start = selected.saturating_sub(count.saturating_sub(1));
        for (i, s) in app.spaces.iter().skip(start).take(count).enumerate() {
            entries.push(Entry {
                rect: Rect {
                    x: 0,
                    y: 1 + i as u16,
                    width,
                    height: 1,
                },
                text: s.name.clone(),
                target: Some(Target::Space(s.id)),
                active: Some(s.id) == app.space,
            });
        }
        entries.push(Entry {
            rect: Rect {
                x: 0,
                y: split - 1,
                width,
                height: 1,
            },
            text: "+ New Space".into(),
            target: Some(Target::NewSpace),
            active: false,
        });
        // Shells remain visibly shells; do not fabricate detected-agent states.
        entries.push(Entry {
            rect: Rect {
                x: 0,
                y: split,
                width,
                height: 1,
            },
            text: "Terminals / Agents".into(),
            target: None,
            active: false,
        });
        let count = app.height.saturating_sub(split + 2) as usize;
        let selected = app
            .tabs
            .iter()
            .position(|t| Some(t.id) == app.selected)
            .unwrap_or(0);
        let start = selected.saturating_sub(count.saturating_sub(1));
        for (i, t) in app.tabs.iter().skip(start).take(count).enumerate() {
            let space = app
                .spaces
                .iter()
                .find(|s| s.id == t.space)
                .map_or("", |s| s.name.as_str());
            entries.push(Entry {
                rect: Rect {
                    x: 0,
                    y: split + 1 + i as u16,
                    width,
                    height: 1,
                },
                text: format!(
                    "{} / {}{}",
                    space,
                    t.label,
                    if matches!(t.terminal, TerminalState::Ended(_)) {
                        " (ended)"
                    } else {
                        ""
                    }
                ),
                target: Some(Target::Tab(t.id)),
                active: Some(t.id) == app.selected,
            });
        }
    }
    let tabs: Vec<_> = app
        .tabs
        .iter()
        .filter(|t| Some(t.space) == app.space)
        .collect();
    let current = tabs
        .iter()
        .position(|t| Some(t.id) == app.selected)
        .unwrap_or(0);
    let widths: Vec<_> = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            crate::paint::clean(
                &format!(
                    "{}:{}{}",
                    i + 1,
                    t.label,
                    if matches!(t.terminal, TerminalState::Ended(_)) {
                        " (ended)"
                    } else {
                        ""
                    }
                ),
                usize::MAX,
            )
            .width()
            .saturating_add(2)
            .min(u16::MAX as usize) as u16
        })
        .collect();
    let available = body.width.saturating_sub(4);
    let mut start = 0;
    while start < current
        && widths[start..=current]
            .iter()
            .map(|w| *w as usize + 1)
            .sum::<usize>()
            > available as usize
    {
        start += 1;
    }
    let mut x = body.x;
    for i in start..tabs.len() {
        let width = widths[i].min(body.x.saturating_add(available).saturating_sub(x));
        if width == 0 {
            break;
        }
        let t = tabs[i];
        let active = Some(t.id) == app.selected;
        let name = format!(
            "{}:{}{}",
            i + 1,
            t.label,
            if matches!(t.terminal, TerminalState::Ended(_)) {
                " (ended)"
            } else {
                ""
            }
        );
        entries.push(Entry {
            rect: Rect {
                x,
                y: 0,
                width,
                height: 1,
            },
            text: if active {
                format!("[{name}]")
            } else {
                format!(" {name} ")
            },
            target: Some(Target::Tab(t.id)),
            active,
        });
        x = x.saturating_add(width + 1);
    }
    if body.width >= 4 {
        entries.push(Entry {
            rect: Rect {
                x: app.width - 4,
                y: 0,
                width: 4,
                height: 1,
            },
            text: " + ".into(),
            target: Some(Target::NewTab),
            active: false,
        });
    }
    Layout {
        body,
        sidebar,
        entries,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sheepdog_core::{Geometry, Server};
    #[test]
    fn sidebar_topbar_and_terminal_have_disjoint_geometry() {
        let mut model = Server::default();
        let space = model.create_space("Work".into(), "/tmp".into()).unwrap();
        let tab = model
            .create_tab(space, "Shell".into(), Geometry::new(54, 23).unwrap())
            .unwrap();
        let mut app = App::new(80, 24);
        app.snapshot(
            model.spaces().cloned().collect(),
            model.tabs().cloned().collect(),
        );
        let layout = compute(&app);
        assert_eq!(
            layout.body,
            Rect {
                x: 26,
                y: 1,
                width: 54,
                height: 23
            }
        );
        assert!(
            layout
                .entries
                .iter()
                .all(|e| !layout.body.contains(e.rect.x, e.rect.y))
        );
        assert!(
            layout
                .entries
                .iter()
                .any(|e| e.rect.contains(77, 0) && e.target == Some(Target::NewTab))
        );
        assert!(
            layout
                .entries
                .iter()
                .any(|e| e.rect.contains(2, 1) && e.target == Some(Target::Space(space)))
        );
        assert!(
            layout
                .entries
                .iter()
                .any(|e| e.target == Some(Target::Tab(tab.tab)) && e.rect.x == 0)
        );
    }
    #[test]
    fn narrow_viewports_keep_positive_terminal_dimensions() {
        let app = App::new(20, 10);
        assert_eq!(
            compute(&app).body,
            Rect {
                x: 0,
                y: 1,
                width: 20,
                height: 9
            }
        );
    }
}
