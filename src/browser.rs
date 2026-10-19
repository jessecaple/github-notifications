//! Own a bounded set of URL-launcher helpers, without a waiter thread per click.

use std::io;
use std::process::{Child, Command};

const MAX_OPENERS: usize = 8;

#[derive(Default)]
pub struct Browser {
    children: Vec<Child>,
}

impl Browser {
    pub fn open(&mut self, url: &str) -> bool {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        match self.launch(&mut command) {
            Ok(true) => return true,
            Ok(false) => eprintln!("URL launchers are still busy; try opening {url} again"),
            Err(e) => eprintln!("Couldn't open {url}: {e}"),
        }
        false
    }

    fn launch(&mut self, command: &mut Command) -> io::Result<bool> {
        self.reap();
        if self.children.len() >= MAX_OPENERS {
            return Ok(false);
        }
        self.children.push(command.spawn()?);
        Ok(true)
    }

    pub fn reap(&mut self) {
        self.children.retain_mut(|child| match child.try_wait() {
            Ok(Some(_)) => false,
            // Retain ownership even on errors so shutdown can still reap it.
            Ok(None) | Err(_) => true,
        });
    }
}

// Do not kill launchers on app exit: xdg-open may exec the user's browser.
// main owns Browser until process exit, when any survivors are reparented.

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[test]
    fn stuck_launchers_are_bounded() {
        let mut browser = Browser::default();
        let mut command = Command::new("sh");
        // No descendants or GUI: each shell waits for its owned stdin pipe.
        command.args(["-c", "read ignored"]).stdin(Stdio::piped());
        for _ in 0..MAX_OPENERS {
            assert!(browser.launch(&mut command).unwrap());
        }
        assert!(!browser.launch(&mut command).unwrap());
        let pids: Vec<_> = browser.children.iter().map(Child::id).collect();
        // Terminate only these isolated test shells, never a user's browser.
        for child in &mut browser.children {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        browser.reap();
        assert!(browser.children.is_empty());
        for pid in pids {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
    }

    #[test]
    fn finished_launchers_release_slots_and_failed_spawns_do_not_take_one() {
        let mut browser = Browser::default();
        assert!(
            browser
                .launch(Command::new("sh").args(["-c", "exit 0"]))
                .unwrap()
        );
        browser.children[0].wait().unwrap();
        browser.reap();
        assert!(browser.children.is_empty());
        assert!(
            browser
                .launch(&mut Command::new(
                    "/nonexistent/github-notifications-opener"
                ))
                .is_err()
        );
        assert!(browser.children.is_empty());
    }
}
