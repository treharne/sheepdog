use crate::{
    app::{Action, App, Effect, Modal, filtered},
    output::{Output, Terminal},
    paint,
};
use anyhow::{Result, bail, ensure};
use crossterm::event::{Event, EventStream, MouseButton, MouseEventKind};
use futures_util::StreamExt;
use sheepdog_client::{Client, View, ViewSnapshot};
use sheepdog_core::{Space, SpaceId, Tab, TabId, TerminalId, TerminalState};
use sheepdog_proto::{Request, Response};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::task::JoinHandle;

struct Operation(JoinHandle<Result<Change>>);
impl Drop for Operation {
    fn drop(&mut self) {
        self.0.abort();
    }
}
enum Message {
    Quit,
    Event(Event),
    Done(std::result::Result<Result<Change>, tokio::task::JoinError>),
    Tick,
}

struct Change {
    spaces: Vec<Space>,
    tabs: Vec<Tab>,
    selected: Option<TabId>,
    space: Option<SpaceId>,
    view: Option<View>,
    attach: bool,
}
async fn perform(
    client: Arc<Client>,
    action: Action,
    geometry: sheepdog_core::Geometry,
    never: bool,
    mut selected: Option<TabId>,
    mut space: Option<SpaceId>,
) -> Result<Change> {
    let attach = !matches!(
        action,
        Action::Refresh
            | Action::EnsureDefault
            | Action::CloseTab(_)
            | Action::CloseSpace(_)
            | Action::RenameSpace(..)
            | Action::RenameTab(..)
    );
    match action {
        Action::Refresh => {}
        Action::EnsureDefault => {
            client.request(Request::EnsureDefault { geometry }).await?;
        }
        Action::RenameSpace(id, name) => {
            client
                .request(Request::RenameSpace { space: id, name })
                .await?;
        }
        Action::RenameTab(id, label) => {
            client
                .request(Request::RenameTab { tab: id, label })
                .await?;
        }
        Action::Select(tab) => selected = Some(tab),
        Action::NewTab(id) => {
            let Response::Tab(tab) = client
                .request(Request::CreateTab {
                    space: id,
                    label: "Shell".into(),
                    geometry,
                })
                .await?
            else {
                bail!("expected Tab");
            };
            selected = Some(tab.id);
            space = Some(id);
        }
        Action::NewSpace(source) => {
            let Response::Tab(tab) = client
                .request(Request::CreateWorkspace { source, geometry })
                .await?
            else {
                bail!("expected starter Tab");
            };
            space = Some(tab.space);
            selected = Some(tab.id);
        }
        Action::CloseTab(tab) => {
            client
                .request(Request::CloseTab {
                    tab,
                    confirmed: true,
                })
                .await?;
        }
        Action::CloseSpace(id) => {
            client
                .request(Request::CloseSpace {
                    space: id,
                    confirmed: true,
                })
                .await?;
        }
        Action::Reopen(tab) => {
            client.request(Request::ReopenTab { tab }).await?;
            selected = Some(tab);
        }
    }
    let Response::Snapshot { spaces, tabs, .. } = client.request(Request::Snapshot).await? else {
        bail!("expected organization");
    };
    if let Some(tab) = selected.and_then(|id| tabs.iter().find(|t| t.id == id)) {
        space = Some(tab.space);
    }
    if !spaces.iter().any(|s| Some(s.id) == space) {
        space = spaces.first().map(|s| s.id);
        selected = None;
    }
    if !tabs.iter().any(|t| Some(t.id) == selected) {
        selected = tabs.iter().find(|t| Some(t.space) == space).map(|t| t.id);
    }
    let view = if attach
        && let Some(tab) = selected.and_then(|id| tabs.iter().find(|t| t.id == id))
        && matches!(tab.terminal, TerminalState::Running(_))
    {
        Some(client.attach(tab.id, geometry, never).await?)
    } else {
        None
    };
    Ok(Change {
        spaces,
        tabs,
        selected,
        space,
        view,
        attach,
    })
}
fn generation(app: &App) -> Option<TerminalId> {
    app.tab().and_then(|t| match t.terminal {
        TerminalState::Running(id) => Some(id),
        _ => None,
    })
}
fn same(a: &Option<ViewSnapshot>, b: &Option<ViewSnapshot>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.status == b.status
                && a.unacked_bytes == b.unacked_bytes
                && match (&a.frame, &b.frame) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                }
        }
        _ => false,
    }
}
fn mouse(app: &mut App, _host: &str, event: crossterm::event::MouseEvent) -> Effect {
    if event.column >= app.width || event.row >= app.height {
        return Effect::None;
    }
    if let Some(Modal::Picker {
        choices,
        cursor,
        filter,
        ..
    }) = &mut app.modal
    {
        let choices = filtered(choices, &filter.text);
        let count = app.height.saturating_sub(3) as usize;
        match event.kind {
            MouseEventKind::ScrollUp => *cursor = cursor.saturating_sub(3),
            MouseEventKind::ScrollDown => {
                *cursor = (*cursor + 3).min(choices.len().saturating_sub(1))
            }
            MouseEventKind::Down(MouseButton::Left) if event.row >= 3 => {
                let start = cursor.saturating_sub(count.saturating_sub(1));
                let i = start + (event.row - 3) as usize;
                if i < choices.len() && event.row < app.height - 1 {
                    *cursor = i;
                    return app.key(crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Enter,
                        crossterm::event::KeyModifiers::NONE,
                    ));
                }
            }
            _ => {}
        }
        return Effect::None;
    }
    if app.modal.is_some() {
        return Effect::None;
    }
    if app.busy {
        if matches!(event.kind, MouseEventKind::Down(_)) {
            app.notice = "Control operation pending; click again when ready".into();
        }
        return Effect::None;
    }
    let target = crate::layout::compute(app)
        .entries
        .iter()
        .find(|e| e.rect.contains(event.column, event.row))
        .and_then(|e| e.target);
    use crate::layout::Target;
    if event.kind == MouseEventKind::Down(MouseButton::Right) {
        match target {
            Some(Target::Space(id)) => {
                if let Some(s) = app.spaces.iter().find(|s| s.id == id) {
                    let mut f = crate::input::Field::default();
                    f.insert(&s.name);
                    app.modal = Some(Modal::Rename(crate::app::RenameTarget::Space(id), f));
                }
            }
            Some(Target::Tab(id)) => {
                if let Some(t) = app.tabs.iter().find(|t| t.id == id) {
                    let mut f = crate::input::Field::default();
                    f.insert(&t.label);
                    app.modal = Some(Modal::Rename(crate::app::RenameTarget::Tab(id), f));
                }
            }
            _ => {}
        }
        return Effect::None;
    }
    if event.kind == MouseEventKind::Down(MouseButton::Left) {
        return match target {
            Some(Target::Space(id)) => app.choose_space(id),
            Some(Target::Tab(id)) => app.choose(id),
            Some(Target::NewSpace) => Effect::Work(Action::NewSpace(app.space)),
            Some(Target::NewTab) => app
                .space
                .map_or(Effect::Work(Action::NewSpace(None)), |space| {
                    Effect::Work(Action::NewTab(space))
                }),
            None => Effect::None,
        };
    }
    if event.kind == MouseEventKind::Down(MouseButton::Middle)
        && let Some(Target::Tab(id)) = target
        && let Some(t) = app.tabs.iter().find(|t| t.id == id)
    {
        app.modal = Some(Modal::ConfirmTab(id, t.label.clone()));
    }
    Effect::None
}

/// Local navigation/UI only. No Sheepdog TUI is launched in a remote shell.
pub async fn run(client: Arc<Client>, host: &str, never: bool) -> Result<()> {
    use std::io::IsTerminal;
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "Sheepdog requires a local interactive terminal"
    );
    let size = crossterm::terminal::size()?;
    let mut app = App::new(size.0, size.1);
    let initial = perform(
        client.clone(),
        Action::EnsureDefault,
        app.geometry(),
        never,
        None,
        None,
    )
    .await?;
    app.space = initial.space;
    app.selected = initial.selected;
    app.snapshot(initial.spaces, initial.tabs);
    let result = ui(&client, host, never, &mut app).await;
    // The raw-mode/paint guards have already restored the user's local TTY.
    // Disconnect is best effort under a dead path; work is never destroyed.
    let _ = tokio::time::timeout(Duration::from_millis(500), client.disconnect()).await;
    result
}
async fn ui(client: &Arc<Client>, host: &str, never: bool, app: &mut App) -> Result<()> {
    let mut output = Output::new()?;
    let _terminal = Terminal::enter(&mut output)?;
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut views: BTreeMap<TabId, (Option<TerminalId>, View)> = BTreeMap::new();
    let mut operation: Option<Operation> = None;
    let mut selecting = false;
    let mut background = false;
    let mut pending = Vec::new();
    let mut fenced = false;
    let mut dirty = true;
    let mut previous = None;
    let mut refresh = std::time::Instant::now();
    let mut next = if app.live() && app.usable() {
        app.selected.map(Action::Select)
    } else {
        None
    };
    loop {
        if let Some(action) = next.take() {
            let selection = !matches!(
                action,
                Action::Refresh | Action::RenameSpace(..) | Action::RenameTab(..)
            );
            selecting = matches!(
                action,
                Action::Select(_) | Action::NewTab(_) | Action::NewSpace(_) | Action::Reopen(_)
            );
            if selection {
                for (_, v) in views.values() {
                    v.pause();
                }
                pending.clear();
                fenced = false;
                app.notice.clear();
                app.literal = false;
            }
            background = matches!(action, Action::Refresh);
            app.busy = !background;
            dirty = true;
            let client = client.clone();
            let geo = app.geometry();
            let selected = app.selected;
            let space = app.space;
            operation = Some(Operation(tokio::spawn(async move {
                perform(client, action, geo, never, selected, space).await
            })));
        }
        let completed = async {
            match &mut operation {
                Some(task) => Some((&mut task.0).await),
                None => std::future::pending().await,
            }
        };
        let message = tokio::select! {
            biased;
            _=term.recv()=>Message::Quit,
            _=interrupt.recv()=>Message::Quit,
            event=events.next()=>match event {Some(event)=>Message::Event(event?),None=>Message::Quit},
            result=completed=>Message::Done(result.unwrap()),
            _=ticker.tick()=>Message::Tick,
        };
        match message {
            Message::Quit => break,
            Message::Event(event) => {
                let old = app.selected;
                let old_space = app.space;
                let effect = match event {
                    Event::Key(key) => app.key(key),
                    Event::Resize(w, h) => {
                        app.width = w;
                        app.height = h;
                        if let Some((_, v)) = app.selected.and_then(|id| views.get(&id)) {
                            if app.usable() && app.live() {
                                if let Err(error) = v.resize(app.geometry()) {
                                    app.notice = error.to_string();
                                    fenced = true;
                                } else {
                                    v.resume();
                                }
                            } else if !app.usable() {
                                v.pause();
                            }
                        }
                        Effect::None
                    }
                    Event::FocusGained => {
                        if let Some((_, v)) = app.selected.and_then(|id| views.get(&id))
                            && app.usable()
                            && app.live()
                            && let Err(error) = v.resize(app.geometry())
                        {
                            app.notice = error.to_string();
                            fenced = true;
                        }
                        Effect::None
                    }
                    Event::Mouse(event) => {
                        if crate::layout::compute(app)
                            .body
                            .contains(event.column, event.row)
                            && app.modal.is_none()
                            && app.live()
                            && let Some((_, v)) = app.selected.and_then(|id| views.get(&id))
                        {
                            let _ = v.resize(app.geometry());
                        }
                        mouse(app, host, event)
                    }
                    Event::Paste(text) => {
                        let literal = std::mem::take(&mut app.literal);
                        if literal {
                            app.notice.clear();
                        }
                        if let Some(Modal::Rename(_, field)) = &mut app.modal {
                            field.insert(&text);
                            Effect::None
                        } else if !literal {
                            app.notice =
                                "File Ingest not built yet; Ctrl+B P arms ONE literal paste".into();
                            Effect::None
                        } else {
                            app.literal = false;
                            if let Some((_, v)) = app.selected.and_then(|id| views.get(&id))
                                && app.live()
                                && !selecting
                            {
                                let mut bytes = text.into_bytes();
                                if v.snapshot().mode & quosh_proto::MODE_BRACKETED_PASTE != 0 {
                                    let mut wrapped = b"\x1b[200~".to_vec();
                                    wrapped.append(&mut bytes);
                                    wrapped.extend_from_slice(b"\x1b[201~");
                                    bytes = wrapped;
                                }
                                Effect::Send(bytes)
                            } else {
                                app.notice = "Wait for a live terminal before literal paste".into();
                                Effect::None
                            }
                        }
                    }
                    _ => Effect::None,
                };
                if app.selected != old || app.space != old_space {
                    if let Some((_, v)) = old.and_then(|id| views.get(&id)) {
                        v.pause();
                    }
                    pending.clear();
                    fenced = false;
                    if background && let Some(mut task) = operation.take() {
                        task.0.abort();
                        let _ = (&mut task.0).await;
                    }
                }
                match effect {
                    Effect::Quit => break,
                    Effect::Work(action) => {
                        if background && let Some(mut task) = operation.take() {
                            task.0.abort();
                            let _ = (&mut task.0).await;
                        }
                        if operation.is_none() {
                            next = Some(action);
                        } else {
                            app.notice = "Control operation pending; action not sent".into();
                        }
                    }
                    Effect::Send(bytes) => {
                        if !app.usable() {
                            app.notice = "Resize terminal before sending input".into();
                        } else if fenced {
                            app.notice="Input fenced after error; select another Tab or detach/re-bootstrap".into();
                        } else if selecting
                            || (app.live() && !views.contains_key(&app.selected.unwrap()))
                        {
                            if pending.len().saturating_add(bytes.len()) <= 64 * 1024 {
                                pending.extend(bytes);
                            } else {
                                fenced = true;
                                app.notice = format!(
                                    "Input budget exhausted; {} queued bytes NOT sent",
                                    pending.len()
                                );
                                pending.clear();
                            }
                        } else if app.live()
                            && let Some((_, v)) = app.selected.and_then(|id| views.get(&id))
                        {
                            if let Err(error) = v.input(bytes) {
                                app.notice = error.to_string();
                                fenced = true;
                            }
                        } else {
                            app.notice="Tab ended or unavailable; Ctrl+B Enter explicitly opens a new shell".into();
                        }
                    }
                    Effect::None => {}
                }
                dirty = true;
            }
            Message::Done(result) => {
                operation = None;
                app.busy = false;
                dirty = true;
                refresh = std::time::Instant::now();
                match result? {
                    Ok(change) => {
                        let selection_changed = app.selected != change.selected;
                        app.space = change.space;
                        app.selected = change.selected;
                        app.snapshot(change.spaces, change.tabs);
                        views.retain(|id, (_, v)| {
                            let keep = app.tabs.iter().any(|t| t.id == *id);
                            if !keep {
                                v.stop();
                            }
                            keep
                        });
                        if change.attach
                            && let Some(id) = app.selected
                        {
                            if let Some(v) = change.view {
                                if fenced {
                                    v.stop();
                                } else {
                                    if let Err(error) = v.resize(app.geometry()) {
                                        app.notice = error.to_string();
                                        fenced = true;
                                    }
                                    if !pending.is_empty()
                                        && !fenced
                                        && let Err(error) = v.input(std::mem::take(&mut pending))
                                    {
                                        app.notice = error.to_string();
                                        fenced = true;
                                    }
                                }
                                views.insert(id, (generation(app), v));
                            } else if !pending.is_empty() {
                                app.notice = format!(
                                    "Terminal ended; {} queued bytes NOT sent",
                                    pending.len()
                                );
                                pending.clear();
                            }
                        }
                        if !fenced
                            && app.live()
                            && app.usable()
                            && (selection_changed
                                || views.get(&app.selected.unwrap()).is_none_or(|(id, v)| {
                                    *id != generation(app)
                                        || v.snapshot().status
                                            == sheepdog_client::ViewStatus::Paused
                                }))
                        {
                            next = app.selected.map(Action::Select);
                        }
                    }
                    Err(error) => {
                        app.notice = format!(
                            "{error}; {} queued bytes NOT sent. Refresh before retrying mutations.",
                            pending.len()
                        );
                        pending.clear();
                        if selecting {
                            fenced = true;
                        }
                    }
                }
                selecting = false;
            }
            Message::Tick => {
                output.flush()?;
                let snapshot = app
                    .selected
                    .and_then(|id| views.get(&id))
                    .map(|(_, v)| v.snapshot());
                if !same(&snapshot, &previous) {
                    dirty = true;
                    previous = snapshot;
                }
                if dirty && output.idle() {
                    output.queue(paint::render(app, host, previous.as_ref()))?;
                    output.flush()?;
                    dirty = false;
                }
                if operation.is_none()
                    && next.is_none()
                    && app.modal.is_none()
                    && refresh.elapsed() >= Duration::from_secs(1)
                {
                    next = Some(Action::Refresh);
                }
            }
        }
    }
    if let Some(mut task) = operation {
        task.0.abort();
        let _ = (&mut task.0).await;
    }
    for (_, view) in views.values() {
        view.pause();
    }
    ensure!(pending.len() <= 64 * 1024, "pending input budget violated");
    Ok(())
}
