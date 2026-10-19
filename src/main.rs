//! GitHub Notifications: a GNOME top-bar icon for the GitHub
//! notifications inbox.

mod browser;
mod desktop;
mod github;
mod tray;

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ksni::blocking::TrayMethods;

use browser::Browser;
use desktop::Notifier;
use github::{Client, Item, Poll};
use tray::{InboxTray, State};

pub const APP_ID: &str = env!("CARGO_PKG_NAME");
const INBOX_URL: &str = "https://github.com/notifications";
const MIN_POLL: Duration = Duration::from_secs(60);
/// More new threads than this at once become a single summary pop-up.
const MAX_POPUPS: usize = 3;
const MAX_COMMANDS: usize = 32;

/// Desktop callbacks must not block behind a GitHub request or grow a queue
/// indefinitely. Tray entries remain available to retry if the queue is full.
#[derive(Clone)]
pub struct Commands(mpsc::SyncSender<Cmd>);

impl Commands {
    pub fn send(&self, cmd: Cmd) -> bool {
        match self.0.try_send(cmd) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("Couldn't queue desktop action: {e}");
                false
            }
        }
    }
}

pub enum Target {
    Thread(Item),
    Inbox,
}

pub enum Cmd {
    Open(Target),
    Quit,
}

fn main() {
    let (tx, rx) = mpsc::sync_channel(MAX_COMMANDS);
    let tx = Commands(tx);
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
    let mut browser = Browser::default();

    loop {
        browser.reap();
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
                Cmd::Open(Target::Inbox) => {
                    browser.open(INBOX_URL);
                }
                Cmd::Open(Target::Thread(item)) => {
                    let opened = browser.open(&client.web_url(&item));
                    tray.update(move |t| {
                        if opened {
                            t.items.retain(|i| i.id != item.id);
                        } else if !t.items.iter().any(|i| i.id == item.id) {
                            // A busy or unavailable launcher leaves a retry in the menu.
                            t.items.insert(0, item);
                        }
                    });
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

fn exit(message: &str) -> ! {
    eprintln!("{APP_ID}: {message}");
    std::process::exit(1)
}
