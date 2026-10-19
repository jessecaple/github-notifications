//! Desktop pop-ups over the freedesktop notifications D-Bus API. One
//! connection sends them and one thread listens for clicks.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread;

use zbus::blocking::{Connection, MessageIterator};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::zvariant::Value;

use crate::tray::ICON;
use crate::{APP_ID, Cmd, Commands, Target};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Claimed on the session bus so only one copy runs.
const BUS_NAME: &str = "io.github.jessecaple.GitHubNotifications";
/// Bound both retained targets and our history on the notification server.
const MAX_SHOWN: usize = 64;

#[derive(Default)]
struct Shown {
    owner: String,
    targets: VecDeque<(u32, Target)>,
    registering: bool,
    /// Signals can arrive before Notify returns its id. Keep a bounded window
    /// until that reply identifies which (if any) belong to our new pop-up.
    early: VecDeque<(String, u32, bool)>,
}

impl Shown {
    fn remove(&mut self, id: u32) -> Option<Target> {
        let index = self.targets.iter().position(|(shown, _)| *shown == id)?;
        self.targets.remove(index).map(|(_, target)| target)
    }

    fn insert(&mut self, owner: &str, id: u32, target: Target) -> Option<u32> {
        self.use_owner(owner);
        self.remove(id);
        let evicted = if self.targets.len() == MAX_SHOWN {
            self.targets.pop_front().map(|(id, _)| id)
        } else {
            None
        };
        self.targets.push_back((id, target));
        evicted
    }

    fn use_owner(&mut self, owner: &str) {
        if self.owner != owner {
            // A restarted server has a new id namespace and no old pop-ups.
            self.targets.clear();
            self.owner = owner.to_owned();
        }
    }
}

pub struct Notifier {
    conn: Connection,
    /// Pop-ups still on screen or in the message tray, by server id.
    shown: Arc<Mutex<Shown>>,
    listener: Option<thread::JoinHandle<()>>,
    registration: Mutex<()>,
    tx: Commands,
}

impl Notifier {
    /// Connects to the session bus. Fails if another copy is already running.
    pub fn new(tx: Commands) -> Result<Self, String> {
        let conn = Connection::session().map_err(|e| format!("No D-Bus session: {e}"))?;
        Self::connect(conn, tx)
    }

    fn connect(conn: Connection, tx: Commands) -> Result<Self, String> {
        let reply = conn
            .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
            .map_err(|e| format!("Couldn't claim {BUS_NAME}: {e}"))?;
        if reply != RequestNameReply::PrimaryOwner {
            return Err("Already running".into());
        }

        let rule = format!("type='signal',sender='{SERVICE}',interface='{SERVICE}',path='{PATH}'");
        let signals = MessageIterator::for_match_rule(rule.as_str(), &conn, None)
            .map_err(|e| format!("Couldn't watch notification clicks: {e}"))?;
        let shown: Arc<Mutex<Shown>> = Arc::default();
        let listener = Arc::clone(&shown);
        let commands = tx.clone();
        let listener = thread::spawn(move || {
            for msg in signals.flatten() {
                let header = msg.header();
                let body = msg.body();
                let event = match header.member().map(|m| m.as_str()) {
                    Some("ActionInvoked") => {
                        if let Ok((id, action)) = body.deserialize::<(u32, String)>()
                            && action == "default"
                        {
                            Some((id, true))
                        } else {
                            None
                        }
                    }
                    Some("NotificationClosed") => body
                        .deserialize::<(u32, u32)>()
                        .ok()
                        .map(|(id, _)| (id, false)),
                    _ => None,
                };
                if let Some((id, open)) = event {
                    let Some(owner) = header.sender() else {
                        continue;
                    };
                    let mut shown = listener.lock().unwrap();
                    let target = if owner.as_str() == shown.owner {
                        shown.remove(id)
                    } else {
                        None
                    };
                    if let Some(target) = target {
                        if open {
                            commands.send(Cmd::Open(target));
                        }
                    } else if shown.registering {
                        if shown.early.len() == MAX_SHOWN {
                            shown.early.pop_front();
                        }
                        shown.early.push_back((owner.to_string(), id, open));
                    }
                }
            }
        });
        Ok(Self {
            conn,
            shown,
            listener: Some(listener),
            registration: Mutex::new(()),
            tx,
        })
    }

    /// Shows a pop-up; clicking it sends `Cmd::Open(target)`.
    pub fn show(&self, summary: &str, body: &str, target: Target) {
        let hints = HashMap::from([("desktop-entry", Value::from(APP_ID))]);
        let _registration = self.registration.lock().unwrap();
        self.shown.lock().unwrap().registering = true;
        // Never hold shown across a D-Bus call: its signal queue must keep
        // draining while the server prepares our reply.
        let reply = self.conn.call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE),
            "Notify",
            &(
                "GitHub Notifications",
                0u32,
                ICON,
                summary,
                escape_markup(body),
                vec!["default", "Open"],
                hints,
                -1i32,
            ),
        );
        let mut shown = self.shown.lock().unwrap();
        shown.registering = false;
        let early = std::mem::take(&mut shown.early);
        match reply.and_then(|msg| {
            Ok((
                msg.header()
                    .sender()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                msg.body().deserialize::<u32>()?,
            ))
        }) {
            Ok((owner, id)) => {
                shown.use_owner(&owner);
                let events: Vec<_> = early
                    .iter()
                    .filter(|(sender, signal_id, _)| sender == &owner && *signal_id == id)
                    .collect();
                if !events.is_empty() {
                    drop(shown);
                    if events.iter().any(|(_, _, open)| *open) {
                        self.tx.send(Cmd::Open(target));
                    }
                    return;
                }
                let evicted = shown.insert(&owner, id, target);
                drop(shown);
                if let Some(id) = evicted {
                    // Address the exact server that issued the id, so a restart
                    // cannot close an unrelated pop-up in a new id namespace.
                    if let Err(e) = self.conn.call_method(
                        Some(owner.as_str()),
                        PATH,
                        Some(SERVICE),
                        "CloseNotification",
                        &(id,),
                    ) {
                        eprintln!("Couldn't close old notification: {e}");
                    }
                }
            }
            Err(e) => eprintln!("Couldn't show notification: {e}"),
        }
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        // Break the listener's connection ownership and wake its blocking read.
        let _ = self.conn.clone().close();
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

/// Notification bodies may contain markup, so titles need escaping.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use zbus::blocking::connection::Builder;
    use zbus::zvariant::OwnedValue;

    #[test]
    fn retained_targets_are_bounded_and_server_restarts_reset_ids() {
        let mut shown = Shown::default();
        for id in 1..=MAX_SHOWN as u32 {
            assert_eq!(shown.insert(":1.1", id, Target::Inbox), None);
        }
        assert_eq!(shown.insert(":1.1", 65, Target::Inbox), Some(1));
        assert_eq!(shown.targets.len(), MAX_SHOWN);
        assert!(shown.remove(1).is_none());
        assert!(shown.remove(2).is_some());
        assert!(shown.remove(2).is_none());
        shown.insert(":1.1", 65, Target::Inbox);
        assert_eq!(shown.targets.len(), MAX_SHOWN - 1);
        shown.insert(":1.2", 1, Target::Inbox);
        assert_eq!(shown.targets.len(), 1);
    }

    struct Bus(Child);

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[derive(Default)]
    struct Calls {
        next: u32,
        notifications: Vec<(String, String, Vec<String>, i32)>,
        closed: Vec<u32>,
    }

    struct Server(Arc<Mutex<Calls>>);

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Server {
        #[allow(clippy::too_many_arguments)]
        async fn notify(
            &self,
            app_name: String,
            replaces_id: u32,
            app_icon: String,
            summary: String,
            body: String,
            actions: Vec<String>,
            hints: HashMap<String, OwnedValue>,
            expire_timeout: i32,
            #[zbus(connection)] conn: &zbus::Connection,
        ) -> zbus::fdo::Result<u32> {
            assert_eq!(app_name, "GitHub Notifications");
            assert_eq!(replaces_id, 0);
            assert_eq!(app_icon, ICON);
            assert_eq!(<&str>::try_from(&hints["desktop-entry"]).unwrap(), APP_ID);
            if summary == "failure" {
                return Err(zbus::fdo::Error::Failed("fixture failure".into()));
            }
            let id = {
                let mut calls = self.0.lock().unwrap();
                calls.next += 1;
                calls
                    .notifications
                    .push((summary.clone(), body, actions, expire_timeout));
                calls.next
            };
            if summary == "immediate-close" {
                conn.emit_signal(
                    None::<&str>,
                    PATH,
                    SERVICE,
                    "NotificationClosed",
                    &(id, 2u32),
                )
                .await
                .unwrap();
            }
            Ok(id)
        }

        fn close_notification(&self, id: u32) {
            self.0.lock().unwrap().closed.push(id);
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "notification listener did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn private_bus_exercises_notify_eviction_click_close_and_listener_shutdown() {
        // An isolated bus never reaches the user's shell, tray or pop-ups.
        let mut bus = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let calls: Arc<Mutex<Calls>> = Arc::default();
        let server = Builder::address(address.trim())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(PATH, Server(calls.clone()))
            .unwrap()
            .build()
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(crate::MAX_COMMANDS);
        let conn = Builder::address(address.trim()).unwrap().build().unwrap();
        let notifier = Notifier::connect(conn, Commands(tx)).unwrap();
        let retained = Arc::downgrade(&notifier.shown);

        for _ in 0..=MAX_SHOWN {
            notifier.show("title", "a < b & c", Target::Inbox);
        }
        assert_eq!(notifier.shown.lock().unwrap().targets.len(), MAX_SHOWN);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.closed, [1]);
        assert_eq!(
            calls.notifications[0],
            (
                "title".into(),
                "a &lt; b &amp; c".into(),
                vec!["default".into(), "Open".into()],
                -1
            )
        );
        drop(calls);

        server
            .emit_signal(
                None::<&str>,
                PATH,
                SERVICE,
                "ActionInvoked",
                &(2u32, "default"),
            )
            .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Cmd::Open(Target::Inbox)
        ));
        server
            .emit_signal(
                None::<&str>,
                PATH,
                SERVICE,
                "NotificationClosed",
                &(3u32, 2u32),
            )
            .unwrap();
        wait_for(|| notifier.shown.lock().unwrap().targets.len() == MAX_SHOWN - 2);
        notifier.show("immediate-close", "", Target::Inbox);
        wait_for(|| notifier.shown.lock().unwrap().targets.len() == MAX_SHOWN - 2);
        notifier.show("failure", "", Target::Inbox);
        assert_eq!(notifier.shown.lock().unwrap().targets.len(), MAX_SHOWN - 2);
        drop(notifier);
        assert!(
            retained.upgrade().is_none(),
            "listener retained notification state"
        );
    }
}
