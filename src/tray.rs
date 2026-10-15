//! The top-bar icon and its inbox menu.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::SystemTime;
use std::{env, fs};

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Status, ToolTip};

use crate::github::Item;
use crate::{APP_ID, Cmd, Target};

pub const ICON: &str = "github-notifications-symbolic";
const ICON_UNREAD: &str = "github-notifications-unread";
/// Longest menu; the rest is one click away on github.com.
const MAX_ITEMS: usize = 25;
const MAX_LABEL_CHARS: usize = 70;

/// Installs named icons: the idle symbolic glyph follows the panel theme,
/// while the regular unread icon preserves its pink color.
pub fn install_icons() {
    let icons = [
        (ICON, include_str!("../icons/github-notifications-symbolic.svg")),
        (ICON_UNREAD, include_str!("../icons/github-notifications-unread.svg")),
    ];
    let data_home = match env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share"),
    };
    let theme = data_home.join("icons/hicolor");
    let dir = theme.join("scalable/status");
    let result = fs::create_dir_all(&dir).and_then(|()| {
        let mut changed = false;
        for (name, svg) in icons {
            let path = dir.join(format!("{name}.svg"));
            if fs::read_to_string(&path).ok().as_deref() != Some(svg) {
                fs::write(path, svg)?;
                changed = true;
            }
        }
        // GNOME Shell rescans a theme only when the theme folder itself
        // changes, not when a file lands in a subfolder.
        if changed {
            fs::File::open(&theme)?.set_modified(SystemTime::now())?;
        }
        Ok(())
    });
    if let Err(e) = result {
        eprintln!("Couldn't install icons into {}: {e}", dir.display());
    }
}

pub enum State {
    Loading,
    Ready,
    Failed(String),
}

pub struct InboxTray {
    pub items: Vec<Item>,
    pub state: State,
    tx: Sender<Cmd>,
}

impl InboxTray {
    pub fn new(tx: Sender<Cmd>) -> Self {
        Self { items: Vec::new(), state: State::Loading, tx }
    }

    fn summary(&self) -> String {
        match (&self.state, self.items.len()) {
            (State::Loading, _) => "Checking GitHub…".into(),
            (State::Failed(e), 0) => e.clone(),
            (_, 0) => "Nothing new".into(),
            (_, 1) => "1 unread notification".into(),
            (_, n) => format!("{n} unread notifications"),
        }
    }
}

impl ksni::Tray for InboxTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "GitHub notifications".into()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_name(&self) -> String {
        if self.items.is_empty() { ICON } else { ICON_UNREAD }.into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: self.summary(), ..Default::default() }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu = vec![label(&self.summary())];
        if let (State::Failed(e), false) = (&self.state, self.items.is_empty()) {
            menu.push(label(e));
        }

        // Group by repository, keeping the inbox's newest-first order.
        let mut groups: Vec<(&str, Vec<&Item>)> = Vec::new();
        for item in self.items.iter().take(MAX_ITEMS) {
            match groups.iter_mut().find(|(repo, _)| *repo == item.repo) {
                Some((_, items)) => items.push(item),
                None => groups.push((&item.repo, vec![item])),
            }
        }
        for (repo, items) in groups {
            menu.push(MenuItem::Separator);
            menu.push(label(repo));
            for item in items {
                menu.push(entry(item));
            }
        }
        if self.items.len() > MAX_ITEMS {
            menu.push(action(&format!("…and {} more", self.items.len() - MAX_ITEMS), |tx| {
                let _ = tx.send(Cmd::Open(Target::Inbox));
            }));
        }

        menu.push(MenuItem::Separator);
        menu.push(action("Open inbox on GitHub", |tx| {
            let _ = tx.send(Cmd::Open(Target::Inbox));
        }));
        menu.push(action("Quit", |tx| {
            let _ = tx.send(Cmd::Quit);
        }));
        menu
    }
}

/// A clickable thread; it leaves the menu straight away since opening it marks it read.
fn entry(item: &Item) -> MenuItem<InboxTray> {
    let mut text = format!("{} · {}", item.kind_label(), item.title);
    if let Some(reason) = item.reason_label() {
        text = format!("{text} ({reason})");
    }
    let item = item.clone();
    StandardItem {
        label: escape_mnemonics(&truncate(&text)),
        activate: Box::new(move |tray: &mut InboxTray| {
            tray.items.retain(|i| i.id != item.id);
            let _ = tray.tx.send(Cmd::Open(Target::Thread(item.clone())));
        }),
        ..Default::default()
    }
    .into()
}

fn action(text: &str, run: impl Fn(&Sender<Cmd>) + Send + 'static) -> MenuItem<InboxTray> {
    StandardItem {
        label: escape_mnemonics(text),
        activate: Box::new(move |tray: &mut InboxTray| run(&tray.tx)),
        ..Default::default()
    }
    .into()
}

fn label(text: &str) -> MenuItem<InboxTray> {
    StandardItem { label: escape_mnemonics(&truncate(text)), enabled: false, ..Default::default() }.into()
}

fn truncate(text: &str) -> String {
    match text.char_indices().nth(MAX_LABEL_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Menu labels treat `_` as an access-key marker; `__` shows a literal underscore.
fn escape_mnemonics(text: &str) -> String {
    text.replace('_', "__")
}
