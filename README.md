# GitHub Notifications

GitHub Notifications shows your GitHub notifications inbox in the
GNOME top bar. It's a native Rust program that idles at about 8 MB of memory.

When a thread in your inbox is new or has new activity, GitHub Notifications
shows a desktop notification, or a single summary when more than three arrive
at once.
Clicking the top-bar icon lists unread threads grouped by repository, or shows
"Nothing new". The icon turns pink while anything is unread and uses a neutral
outline when clear.

Clicking a notification or a menu entry opens the page GitHub's own inbox
would: the latest comment on an issue or pull request, or the exact workflow
run for a CI notification. GitHub marks the thread read when that page loads.

## Requirements

- GNOME with the
  [AppIndicator extension](https://github.com/ubuntu/gnome-shell-extension-appindicator)
  enabled. On Fedora it's the `gnome-shell-extension-appindicator` package.
- A GitHub token, from either of:
  - the [GitHub CLI](https://cli.github.com/), signed in with `gh auth login`
  - a classic personal access token in the `GITHUB_NOTIFICATIONS_TOKEN` environment
    variable, with the `notifications` scope. Add `repo` if you want links
    into private repositories to reach the exact comment or run. GitHub's
    notifications API doesn't accept fine-grained tokens.
- Rust 1.89 or newer, to build.

## Install

```sh
git clone https://github.com/jessecaple/github-notifications.git
cd github-notifications
./install.sh
```

The script builds a release binary, installs it to `~/.local/bin/github-notifications`, adds
GitHub Notifications to the app menu and your login items, and starts it. To upgrade, pull and
run `./install.sh` again.

At login, GitHub Notifications waits for the AppIndicator extension to become ready before
its icon appears.

To remove everything it installed:

```sh
./install.sh --uninstall
```

## How it works

GitHub Notifications polls `GET /notifications` every 60 seconds, or less often if GitHub's
`X-Poll-Interval` header asks for that. After the first poll, each one sends
`If-Modified-Since`, and when nothing has changed GitHub answers
`304 Not Modified`, which doesn't count against your rate limit.

The only other API calls happen when you click a thread. GitHub Notifications looks up that
thread's page, fetches your user ID the first time, and adds the
`notification_referrer_id` parameter that github.com puts on its own inbox
links.

GitHub Notifications doesn't store your token. It reads `GITHUB_NOTIFICATIONS_TOKEN`, or runs
`gh auth token` when it starts. The previous `TBGN_GITHUB_TOKEN` variable
continues to work as a fallback. If GitHub rejects the token, it runs
`gh auth token` again, so a `gh auth refresh` takes effect without a restart.

Each time it starts, GitHub Notifications writes its two icons to
`~/.local/share/icons/hicolor/scalable/status/` if they're missing or out of
date. The idle symbolic icon follows the panel theme; the unread icon
preserves its pink color.

## Limitations

- It shows unread threads only. The menu lists up to 25 and GitHub Notifications fetches up to
  200; "Open inbox on GitHub" shows the rest.
- It works with github.com only, not GitHub Enterprise Server.
- One copy runs per session.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo run
```

## License

[MIT](LICENSE). GitHub Notifications isn't affiliated with or endorsed by GitHub.
