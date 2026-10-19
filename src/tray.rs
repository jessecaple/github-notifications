//! The top-bar icon and its inbox menu.

use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;
use std::{env, fs};

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Status, ToolTip};

use crate::github::Item;
use crate::{APP_ID, Cmd, Commands, Target};

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
            if !fs::File::open(&path).is_ok_and(|file| icon_matches(file, svg)) {
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

/// Read only enough to compare our small embedded icon, even if the existing
/// file is unexpectedly large or grows while we read it.
fn icon_matches(reader: impl Read, expected: &str) -> bool {
    let mut bytes = Vec::new();
    reader
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .is_ok()
        && bytes == expected.as_bytes()
}

pub enum State {
    Loading,
    Ready,
    Failed(String),
}

pub struct InboxTray {
    pub items: Vec<Item>,
    pub state: State,
    tx: Commands,
}

impl InboxTray {
    pub fn new(tx: Commands) -> Self {
        Self {
            items: Vec::new(),
            state: State::Loading,
            tx,
        }
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
            if tray.tx.send(Cmd::Open(Target::Thread(item.clone()))) {
                tray.items.retain(|i| i.id != item.id);
            }
        }),
        ..Default::default()
    }
    .into()
}

fn action(text: &str, run: impl Fn(&Commands) + Send + 'static) -> MenuItem<InboxTray> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray;
    use std::sync::mpsc;

    #[test]
    fn existing_icon_comparison_does_not_read_an_oversized_file() {
        let mut input = std::io::Cursor::new(vec![b'x'; 4096]);
        assert!(!icon_matches(&mut input, "xxx"));
        assert_eq!(input.position(), 4);
        assert!(icon_matches(&b"<svg/>"[..], "<svg/>"));
        assert!(!icon_matches(&b"<svg"[..], "<svg/>"));
    }
    #[test]
    fn saturated_desktop_queue_keeps_thread_available_and_callback_returns() {
        let (tx, rx) = mpsc::sync_channel(crate::MAX_COMMANDS);
        let tx = Commands(tx);
        for _ in 0..crate::MAX_COMMANDS {
            assert!(tx.send(Cmd::Open(Target::Inbox)));
        }
        let item = crate::github::test_item();
        let MenuItem::Standard(entry) = entry(&item) else {
            panic!("not a thread entry")
        };
        assert_eq!(entry.label, "Issue · A thread (mentioned)");
        let mut tray = InboxTray::new(tx);
        tray.items.push(item);
        (entry.activate)(&mut tray);
        assert_eq!(tray.items.len(), 1);
        rx.recv().unwrap();
        (entry.activate)(&mut tray);
        assert!(tray.items.is_empty());
        let queued: Vec<_> = rx.try_iter().collect();
        assert_eq!(queued.len(), crate::MAX_COMMANDS);
        assert!(matches!(queued.last(), Some(Cmd::Open(Target::Thread(item))) if item.id == "123"));
        assert_eq!(tray.icon_name(), ICON);
    }

    #[test]
    #[ignore = "run under dbus-run-session with GITHUB_NOTIFICATIONS_ISOLATED_TEST=1"]
    fn isolated_tray_service_exposes_labels_and_delivers_clicks() {
        assert_eq!(
            std::env::var("GITHUB_NOTIFICATIONS_ISOLATED_TEST").as_deref(),
            Ok("1")
        );
        use ksni::blocking::TrayMethods;
        use std::collections::HashMap;
        use std::time::Duration;
        use zbus::blocking::Connection;
        use zbus::zvariant::{OwnedValue, Structure, Value};

        let (tx, rx) = mpsc::sync_channel(crate::MAX_COMMANDS);
        let mut tray = InboxTray::new(Commands(tx));
        tray.items.push(crate::github::test_item());
        tray.state = State::Ready;
        let handle = tray.assume_sni_available(true).spawn().unwrap();
        let conn = Connection::session().unwrap();
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
        let labels = || -> Vec<(i32, String)> {
            let reply = conn
                .call_method(
                    Some(name.as_str()),
                    "/MenuBar",
                    Some("com.canonical.dbusmenu"),
                    "GetLayout",
                    &(0i32, -1i32, Vec::<String>::new()),
                )
                .unwrap();
            let (_, (_, _, children)) = reply.body().deserialize::<(u32, Layout)>().unwrap();
            children
                .into_iter()
                .filter_map(|child| {
                    let (id, properties, _): Layout =
                        Structure::try_from(child).unwrap().try_into().unwrap();
                    properties
                        .get("label")
                        .map(|label| (id, <&str>::try_from(label).unwrap().to_owned()))
                })
                .collect()
        };
        let click = |id: i32| {
            conn.call_method(
                Some(name.as_str()),
                "/MenuBar",
                Some("com.canonical.dbusmenu"),
                "Event",
                &(id, "clicked", Value::from(""), 0u32),
            )
            .unwrap();
        };
        let menu = labels();
        assert!(
            menu.iter()
                .any(|(_, label)| label == "1 unread notification")
        );
        assert!(menu.iter().any(|(_, label)| label == "octo/hello"));
        let id = menu
            .iter()
            .find(|(_, label)| label == "Issue · A thread (mentioned)")
            .unwrap()
            .0;
        click(id);
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Cmd::Open(Target::Thread(item)) if item.id == "123")
        );
        let menu = labels();
        assert!(menu.iter().any(|(_, label)| label == "Nothing new"));
        click(
            menu.iter()
                .find(|(_, label)| label == "Open inbox on GitHub")
                .unwrap()
                .0,
        );
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Cmd::Open(Target::Inbox)
        ));
        let menu = labels();
        click(menu.iter().find(|(_, label)| label == "Quit").unwrap().0);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Cmd::Quit
        ));
        handle.shutdown().wait();
    }
}
