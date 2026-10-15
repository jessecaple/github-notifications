//! Desktop pop-ups over the freedesktop notifications D-Bus API. One
//! connection sends them and one thread listens for clicks.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use zbus::blocking::{Connection, MessageIterator};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::zvariant::Value;

use crate::tray::ICON;
use crate::{APP_ID, Cmd, Target};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Claimed on the session bus so only one copy runs.
const BUS_NAME: &str = "io.github.jessecaple.GitHubNotifications";

pub struct Notifier {
    conn: Connection,
    /// Pop-ups still on screen or in the message tray, by server id.
    shown: Arc<Mutex<HashMap<u32, Target>>>,
}

impl Notifier {
    /// Connects to the session bus. Fails if another copy is already running.
    pub fn new(tx: Sender<Cmd>) -> Result<Self, String> {
        let conn = Connection::session().map_err(|e| format!("No D-Bus session: {e}"))?;
        let reply = conn
            .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
            .map_err(|e| format!("Couldn't claim {BUS_NAME}: {e}"))?;
        if reply != RequestNameReply::PrimaryOwner {
            return Err("Already running".into());
        }

        let rule = format!("type='signal',sender='{SERVICE}',interface='{SERVICE}',path='{PATH}'");
        let signals = MessageIterator::for_match_rule(rule.as_str(), &conn, None)
            .map_err(|e| format!("Couldn't watch notification clicks: {e}"))?;
        let shown: Arc<Mutex<HashMap<u32, Target>>> = Arc::default();
        let listener = Arc::clone(&shown);
        thread::spawn(move || {
            for msg in signals.flatten() {
                let header = msg.header();
                let body = msg.body();
                match header.member().map(|m| m.as_str()) {
                    Some("ActionInvoked") => {
                        if let Ok((id, action)) = body.deserialize::<(u32, String)>()
                            && action == "default"
                            && let Some(target) = listener.lock().unwrap().remove(&id)
                        {
                            let _ = tx.send(Cmd::Open(target));
                        }
                    }
                    Some("NotificationClosed") => {
                        if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>() {
                            listener.lock().unwrap().remove(&id);
                        }
                    }
                    _ => {}
                }
            }
        });
        Ok(Self { conn, shown })
    }

    /// Shows a pop-up; clicking it sends `Cmd::Open(target)`.
    pub fn show(&self, summary: &str, body: &str, target: Target) {
        let hints = HashMap::from([("desktop-entry", Value::from(APP_ID))]);
        let reply = self.conn.call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE),
            "Notify",
            &("GitHub Notifications", 0u32, ICON, summary, escape_markup(body), vec!["default", "Open"], hints, -1i32),
        );
        match reply.and_then(|msg| msg.body().deserialize::<u32>()) {
            Ok(id) => {
                self.shown.lock().unwrap().insert(id, target);
            }
            Err(e) => eprintln!("Couldn't show notification: {e}"),
        }
    }
}

/// Notification bodies may contain markup, so titles need escaping.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
