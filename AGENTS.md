# Agent notes

The README covers what GitHub Notifications does and how to install it. This file holds what
the code and config don't tell you.

## Verifying a change

Unit tests cover parsing and resource cleanup; pop-up fixtures use a private D-Bus server.
Test real tray menu labels and click delivery on an isolated session bus with:

```sh
dbus-run-session -- env GITHUB_NOTIFICATIONS_ISOLATED_TEST=1 cargo test --locked isolated_tray_service_exposes_labels_and_delivers_clicks -- --ignored
```

For any change to the tray, pop-ups or links,
run the app and observe it. You're done when the menu labels, the URLs a click
opens, and the `Notify` calls all match what you expect.

1. Stop the installed copy with `pkill -f -x "$HOME/.local/bin/github-notifications"`. The session bus name in
   `desktop.rs` allows one copy at a time.
2. If the inbox is empty, test against read threads: in a scratch copy of the
   repo, change the poll URL to `notifications?all=true&per_page=3` and
   `MAX_PAGES` to 1. Keep that edit out of commits.
3. Put a stand-in `xdg-open` first on `PATH` that appends `"$@"` to a log, so
   clicks record their URL instead of opening a browser.
4. Read the menu with
   `busctl --user call org.kde.StatusNotifierItem-<pid>-1 /MenuBar com.canonical.dbusmenu GetLayout iias -- 0 -1 0`
   and click an item with `... Event isvu <id> clicked s "" 0`. Item IDs
   change whenever the layout changes, so re-read the menu before each click.
5. Watch pop-ups with
   `dbus-monitor --session "interface='org.freedesktop.Notifications'"`.
   The click listener accepts signals only from the notification server, so
   testing a pop-up click takes a real mouse click.
6. Run `./install.sh` to restore the installed copy.

## Icons

- Draw icons with filled shapes only, using `fill-rule="evenodd"` within one
  path for holes. GNOME recolors a symbolic icon by forcing a fill on every
  shape, so a stroke renders as a filled region.
- After changing an icon, re-run `./install.sh`; `tray::install_icons`
  explains why icons are served by name from the user icon theme.

## Dependencies

- All D-Bus work runs on zbus's `async-io` backend: `ksni` is built without
  default features, which would pull in tokio. Keep every dependency on
  that one runtime so the app stays small and light at idle.
- Desktop notifications are hand-written over zbus so one connection and one
  listener thread serve every pop-up.
- `rust-version` in `Cargo.toml` tracks the highest minimum Rust version among
  dependencies; raise it when an upgrade raises theirs.

## GitHub API

- The notifications API accepts classic tokens and `gh`'s OAuth token only,
  never fine-grained tokens.
- Each poll is one conditional request, so an unchanged inbox costs no rate
  limit. Put any extra lookups on the click path.
- CI threads have no subject URL. `github::CheckSuite` parses GitHub's title
  text to find the run, and falls back to a filtered Actions page when the
  title doesn't match.
