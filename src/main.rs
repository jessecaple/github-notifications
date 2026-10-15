//! GitHub Notifications: a GNOME top-bar icon for the GitHub
//! notifications inbox.

mod desktop;
mod github;
mod tray;

use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ksni::blocking::TrayMethods;

use desktop::Notifier;
use github::{Client, Item, Poll};
use tray::{InboxTray, State};

pub const APP_ID: &str = env!("CARGO_PKG_NAME");
const INBOX_URL: &str = "https://github.com/notifications";
const MIN_POLL: Duration = Duration::from_secs(60);
/// More new threads than this at once become a single summary pop-up.
const MAX_POPUPS: usize = 3;

pub enum Target {
    Thread(Item),
    Inbox,
}

pub enum Cmd {
    Open(Target),
    Quit,
}

fn main() {
    let (tx, rx) = mpsc::channel();
    let notifier = Notifier::new(tx.clone()).unwrap_or_else(|e| exit(&e));
    tray::install_icons();
    // Login autostart can run before GNOME's AppIndicator extension is ready.
    // Keep the tray service alive so it registers when the watcher appears.
    let tray = InboxTray::new(tx)
        .assume_sni_available(true)
        .spawn()
        .unwrap_or_else(|e| exit(&format!("Couldn't create the tray icon: {e}")));
    let mut client = Client::new();
    let mut seen: Option<HashMap<String, String>> = None;

    loop {
        match client.poll() {
            Ok(Poll::Unchanged) => {
                tray.update(|t| t.state = State::Ready);
            }
            Ok(Poll::Changed(items)) => {
                announce(&notifier, &mut seen, &items);
                tray.update(move |t| {
                    t.items = items;
                    t.state = State::Ready;
                });
            }
            Err(e) => {
                eprintln!("{e}");
                tray.update(move |t| t.state = State::Failed(e));
            }
        }

        let next_poll = Instant::now() + client.poll_interval.max(MIN_POLL);
        while let Ok(cmd) = rx.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            match cmd {
                Cmd::Open(Target::Inbox) => open(INBOX_URL),
                Cmd::Open(Target::Thread(item)) => {
                    open(&client.web_url(&item));
                    tray.update(move |t| t.items.retain(|i| i.id != item.id));
                }
                Cmd::Quit => {
                    tray.shutdown().wait();
                    return;
                }
            }
        }
    }
}

/// Pops up threads that are new or have new activity since the last poll.
fn announce(notifier: &Notifier, seen: &mut Option<HashMap<String, String>>, items: &[Item]) {
    let current = items.iter().map(|i| (i.id.clone(), i.updated_at.clone())).collect();
    let previous = seen.replace(current);
    let fresh: Vec<&Item> = items
        .iter()
        .filter(|i| previous.as_ref().is_none_or(|p| p.get(&i.id) != Some(&i.updated_at)))
        .collect();

    if fresh.len() > MAX_POPUPS {
        notifier.show(&format!("{} new GitHub notifications", fresh.len()), "Click to open your inbox", Target::Inbox);
        return;
    }
    for item in fresh {
        let mut body = format!("{} · {}", item.repo, item.kind_label());
        if let Some(reason) = item.reason_label() {
            body = format!("{body} · {reason}");
        }
        notifier.show(&item.title, &body, Target::Thread(item.clone()));
    }
}

fn open(url: &str) {
    match Command::new("xdg-open").arg(url).spawn() {
        // Reap it so it doesn't linger as a zombie.
        Ok(mut child) => drop(thread::spawn(move || child.wait())),
        Err(e) => eprintln!("Couldn't open {url}: {e}"),
    }
}

fn exit(message: &str) -> ! {
    eprintln!("{APP_ID}: {message}");
    std::process::exit(1)
}
