//! Reads the GitHub notifications inbox over the REST API.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::http::Response;
use ureq::{Agent, Body};

const API: &str = "https://api.github.com";
/// The API serves at most 50 threads per page; four pages is plenty for a menu.
const MAX_PAGES: usize = 4;

/// One unread thread in the inbox.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub repo: String,
    pub title: String,
    pub kind: String,
    pub reason: String,
    pub updated_at: String,
    /// API resource whose `html_url` is the page GitHub's inbox links to.
    follow_url: Option<String>,
    /// Page to open when `follow_url` is missing or can't be resolved.
    fallback_url: String,
}

impl Item {
    /// Short name for the subject type, e.g. "PR".
    pub fn kind_label(&self) -> &str {
        match self.kind.as_str() {
            "PullRequest" => "PR",
            "CheckSuite" | "WorkflowRun" => "CI",
            "RepositoryVulnerabilityAlert" | "RepositoryDependabotAlertsThread" => "Security",
            other => other,
        }
    }

    /// Why this thread is in the inbox, when that's worth showing.
    pub fn reason_label(&self) -> Option<&'static str> {
        match self.reason.as_str() {
            "review_requested" => Some("review requested"),
            "mention" | "team_mention" => Some("mentioned"),
            "assign" => Some("assigned"),
            "approval_requested" => Some("approval requested"),
            "security_alert" => Some("security alert"),
            _ => None,
        }
    }
}

pub enum Poll {
    /// Nothing changed since the last poll.
    Unchanged,
    Changed(Vec<Item>),
}

pub struct Client {
    agent: Agent,
    token: Option<String>,
    user_id: Option<u64>,
    last_modified: Option<String>,
    /// Minimum delay GitHub asks for between polls (`X-Poll-Interval`).
    pub poll_interval: Duration,
}

impl Client {
    pub fn new() -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            agent,
            token: None,
            user_id: None,
            last_modified: None,
            poll_interval: Duration::from_secs(60),
        }
    }

    /// Fetches unread inbox threads. Conditional requests that come back
    /// `304 Not Modified` don't count against the rate limit.
    pub fn poll(&mut self) -> Result<Poll, String> {
        let since = self.last_modified.clone();
        let mut resp = self.get(&format!("{API}/notifications?per_page=50"), since.as_deref())?;
        if resp.status() == 304 {
            return Ok(Poll::Unchanged);
        }
        if let Some(secs) = header(&resp, "x-poll-interval").and_then(|v| v.parse().ok()) {
            self.poll_interval = Duration::from_secs(secs);
        }
        let last_modified = header(&resp, "last-modified");

        let mut items = Vec::new();
        for page in 1..=MAX_PAGES {
            if resp.status() != 200 {
                return Err(status_error(&mut resp));
            }
            let next = next_link(&resp);
            let threads: Vec<Thread> = read_json(&mut resp)?;
            items.extend(threads.into_iter().map(Item::from));
            match next {
                Some(url) if page < MAX_PAGES => resp = self.get(&url, None)?,
                _ => break,
            }
        }
        self.last_modified = last_modified;
        Ok(Poll::Changed(items))
    }

    /// The URL github.com's inbox opens for this thread: the latest comment's
    /// page, tagged with the referrer id that marks the thread read on visit.
    pub fn web_url(&mut self, item: &Item) -> String {
        let url = match item.follow_url.as_deref() {
            Some(url) => self.html_url(url),
            None => self.workflow_run_url(item),
        };
        let url = url.unwrap_or_else(|| item.fallback_url.clone());
        match self.user_id() {
            Some(user_id) => with_referrer(&url, &item.id, user_id),
            None => url,
        }
    }

    fn html_url(&mut self, api_url: &str) -> Option<String> {
        let mut resp = self.get(api_url, None).ok()?;
        if resp.status() != 200 {
            return None;
        }
        read_json::<HtmlUrl>(&mut resp).ok().map(|r| r.html_url)
    }

    /// CI threads carry no API link, only a title like "ci workflow run failed
    /// for main branch". The run is the newest match that finished before the
    /// notification.
    fn workflow_run_url(&mut self, item: &Item) -> Option<String> {
        let run = CheckSuite::parse(&item.title)?;
        let mut url = format!("{API}/repos/{}/actions/runs?per_page=30&branch={}", item.repo, encode(run.branch));
        if let Some(status) = run.status {
            url.push_str(&format!("&status={status}"));
        }
        let mut resp = self.get(&url, None).ok()?;
        if resp.status() != 200 {
            return None;
        }
        read_json::<WorkflowRuns>(&mut resp)
            .ok()?
            .workflow_runs
            .into_iter()
            .filter(|r| r.name.as_deref() == Some(run.workflow) && r.updated_at <= item.updated_at)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
            .map(|r| r.html_url)
    }

    fn user_id(&mut self) -> Option<u64> {
        if self.user_id.is_none() {
            let mut resp = self.get(&format!("{API}/user"), None).ok()?;
            if resp.status() == 200 {
                self.user_id = read_json::<User>(&mut resp).ok().map(|u| u.id);
            }
        }
        self.user_id
    }

    /// GETs `url`, reloading the token once if GitHub rejects it (e.g. after
    /// `gh auth refresh`).
    fn get(&mut self, url: &str, if_modified_since: Option<&str>) -> Result<Response<Body>, String> {
        let resp = self.send_get(url, if_modified_since)?;
        if resp.status() != 401 {
            return Ok(resp);
        }
        self.token = None;
        self.send_get(url, if_modified_since)
    }

    fn send_get(&mut self, url: &str, if_modified_since: Option<&str>) -> Result<Response<Body>, String> {
        let token = match &self.token {
            Some(token) => token.clone(),
            None => self.token.insert(load_token()?).clone(),
        };
        let mut req = self
            .agent
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(since) = if_modified_since {
            req = req.header("If-Modified-Since", since);
        }
        req.call().map_err(|e| format!("Couldn't reach GitHub: {e}"))
    }
}

/// Uses the configured token (including the legacy name), then the CLI login.
fn load_token() -> Result<String, String> {
    let primary = std::env::var("GITHUB_NOTIFICATIONS_TOKEN").ok();
    let legacy = std::env::var("TBGN_GITHUB_TOKEN").ok();
    if let Some(token) = configured_token(primary.as_deref(), legacy.as_deref()) {
        return Ok(token);
    }
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .output()
        .map_err(|e| format!("Couldn't run `gh auth token`: {e}"))?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || token.is_empty() {
        return Err("Not signed in: run `gh auth login`".into());
    }
    Ok(token)
}

/// Prefer the renamed variable, retaining compatibility with existing installs.
fn configured_token(primary: Option<&str>, legacy: Option<&str>) -> Option<String> {
    primary
        .into_iter()
        .chain(legacy)
        .map(str::trim)
        .find(|token| !token.is_empty())
        .map(str::to_owned)
}

#[derive(Deserialize)]
struct Thread {
    id: String,
    reason: String,
    updated_at: String,
    subject: Subject,
    repository: Repository,
}

#[derive(Deserialize)]
struct Subject {
    title: String,
    url: Option<String>,
    latest_comment_url: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
    html_url: String,
}

#[derive(Deserialize)]
struct HtmlUrl {
    html_url: String,
}

#[derive(Deserialize)]
struct User {
    id: u64,
}

#[derive(Deserialize)]
struct WorkflowRuns {
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    name: Option<String>,
    html_url: String,
    updated_at: String,
}

/// What a CI thread's title says about its run.
#[derive(Debug, PartialEq)]
struct CheckSuite<'a> {
    workflow: &'a str,
    /// The Actions API `status` filter matching the outcome, when known.
    status: Option<&'static str>,
    branch: &'a str,
}

impl<'a> CheckSuite<'a> {
    /// Parses "{workflow} workflow run[, Attempt #N] {outcome} for {branch} branch".
    fn parse(title: &'a str) -> Option<Self> {
        let (head, branch) = title.strip_suffix(" branch")?.rsplit_once(" for ")?;
        let (workflow, rest) = head.split_once(" workflow run")?;
        let outcome = match rest.strip_prefix(", Attempt #") {
            Some(attempt) => attempt.split_once(' ')?.1,
            None => rest.trim_start(),
        };
        let status = match outcome {
            "failed" | "failed at startup" => Some("failure"),
            "succeeded" => Some("success"),
            "cancelled" => Some("cancelled"),
            "skipped" => Some("skipped"),
            _ => None,
        };
        Some(Self { workflow, status, branch })
    }

    /// The same runs as a filtered Actions page.
    fn actions_url(&self, repo_html: &str) -> String {
        let mut query = format!("workflow:\"{}\" branch:{}", self.workflow, self.branch);
        if let Some(status) = self.status {
            query.push_str(&format!(" is:{status}"));
        }
        format!("{repo_html}/actions?query={}", encode(&query))
    }
}

impl From<Thread> for Item {
    fn from(t: Thread) -> Self {
        let fallback_url = fallback_url(
            t.subject.url.as_deref(),
            &t.subject.kind,
            &t.subject.title,
            &t.repository.full_name,
            &t.repository.html_url,
        );
        Item {
            id: t.id,
            repo: t.repository.full_name,
            title: t.subject.title,
            kind: t.subject.kind,
            reason: t.reason,
            updated_at: t.updated_at,
            follow_url: t.subject.latest_comment_url.or(t.subject.url),
            fallback_url,
        }
    }
}

/// Best web page for a subject without asking the API.
fn fallback_url(subject_url: Option<&str>, kind: &str, title: &str, repo: &str, repo_html: &str) -> String {
    let prefix = format!("{API}/repos/{repo}/");
    if let Some(rest) = subject_url.and_then(|u| u.strip_prefix(&prefix)) {
        match rest.split_once('/') {
            Some(("pulls", n)) => return format!("{repo_html}/pull/{n}"),
            Some(("issues", n)) => return format!("{repo_html}/issues/{n}"),
            Some(("commits", sha)) => return format!("{repo_html}/commit/{sha}"),
            Some(("releases", _)) => return format!("{repo_html}/releases"),
            _ => {}
        }
    }
    match kind {
        "Discussion" => format!("{repo_html}/discussions"),
        "CheckSuite" | "WorkflowRun" => match CheckSuite::parse(title) {
            Some(run) => run.actions_url(repo_html),
            None => format!("{repo_html}/actions"),
        },
        _ => repo_html.to_owned(),
    }
}

/// Percent-encodes a query value, with spaces as `+`.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Adds the `notification_referrer_id` github.com puts on inbox links, keeping
/// any `#fragment` last.
fn with_referrer(url: &str, thread_id: &str, user_id: u64) -> String {
    let referrer = base64(format!("018:NotificationThread{thread_id}:{user_id}").as_bytes());
    let encoded = referrer.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D");
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut out = format!("{base}{sep}notification_referrer_id={encoded}");
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn header(resp: &Response<Body>, name: &str) -> Option<String> {
    resp.headers().get(name)?.to_str().ok().map(str::to_owned)
}

/// The `rel="next"` target of a `Link` pagination header.
fn next_link(resp: &Response<Body>) -> Option<String> {
    let link = header(resp, "link")?;
    let next = link.split(',').find(|part| part.contains("rel=\"next\""))?;
    let url = next.split(';').next()?.trim();
    Some(url.trim_start_matches('<').trim_end_matches('>').to_owned())
}

fn read_json<T: DeserializeOwned>(resp: &mut Response<Body>) -> Result<T, String> {
    let body = resp.body_mut().read_to_string().map_err(|e| format!("Couldn't read GitHub's reply: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("Unexpected reply from GitHub: {e}"))
}

fn status_error(resp: &mut Response<Body>) -> String {
    let status = resp.status();
    let message = resp
        .body_mut()
        .read_to_string()
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|v| v["message"].as_str().map(str::to_owned));
    match message {
        Some(message) => format!("GitHub {status}: {message}"),
        None => format!("GitHub returned {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "octo/hello";
    const HTML: &str = "https://github.com/octo/hello";

    #[test]
    fn renamed_token_setting_takes_precedence_and_preserves_legacy_support() {
        assert_eq!(configured_token(Some(" new "), Some("old")), Some("new".into()));
        assert_eq!(configured_token(Some(" "), Some(" old ")), Some("old".into()));
        assert_eq!(configured_token(None, Some("old")), Some("old".into()));
        assert_eq!(configured_token(None, Some(" ")), None);
    }

    #[test]
    fn fallback_maps_api_subjects_to_pages() {
        let api = |path: &str| format!("{API}/repos/{REPO}/{path}");
        let url = |subject: Option<&str>, kind| fallback_url(subject, kind, "t", REPO, HTML);
        assert_eq!(url(Some(&api("pulls/7")), "PullRequest"), format!("{HTML}/pull/7"));
        assert_eq!(url(Some(&api("issues/8")), "Issue"), format!("{HTML}/issues/8"));
        assert_eq!(url(Some(&api("commits/abc")), "Commit"), format!("{HTML}/commit/abc"));
        assert_eq!(url(Some(&api("releases/99")), "Release"), format!("{HTML}/releases"));
        assert_eq!(url(None, "Discussion"), format!("{HTML}/discussions"));
        assert_eq!(url(None, "CheckSuite"), format!("{HTML}/actions"));
        assert_eq!(url(None, "Other"), HTML);
    }

    #[test]
    fn parses_ci_titles() {
        assert_eq!(
            CheckSuite::parse("ci workflow run failed for master branch"),
            Some(CheckSuite { workflow: "ci", status: Some("failure"), branch: "master" })
        );
        assert_eq!(
            CheckSuite::parse("Build and test workflow run, Attempt #2 succeeded for feat/for branch"),
            Some(CheckSuite { workflow: "Build and test", status: Some("success"), branch: "feat/for" })
        );
        assert_eq!(CheckSuite::parse("Fix the build"), None);
        assert_eq!(
            fallback_url(None, "CheckSuite", "ci workflow run failed for master branch", REPO, HTML),
            format!("{HTML}/actions?query=workflow:%22ci%22+branch:master+is:failure")
        );
    }

    #[test]
    fn referrer_goes_before_fragment() {
        // btoa("018:NotificationThread123:456")
        let id = "MDE4Ok5vdGlmaWNhdGlvblRocmVhZDEyMzo0NTY%3D";
        assert_eq!(
            with_referrer(&format!("{HTML}/pull/7#issuecomment-1"), "123", 456),
            format!("{HTML}/pull/7?notification_referrer_id={id}#issuecomment-1")
        );
        assert_eq!(
            with_referrer(&format!("{HTML}/pull/7?a=b"), "123", 456),
            format!("{HTML}/pull/7?a=b&notification_referrer_id={id}")
        );
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
    }
}
