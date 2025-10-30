use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet, VecDeque};
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Cursor, Read, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use arboard::Clipboard;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::cursor::MoveTo;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::style::Print;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, window_size, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use image::{self, ImageFormat};
use once_cell::sync::Lazy;
use percent_encoding::percent_decode_str;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use ratatui::{Frame, Terminal};
use reqwest::{blocking::Client, header::CONTENT_TYPE, Error as ReqwestError};
use semver::Version;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use url::Url;

use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use regex::{Captures, Regex};
use textwrap::{wrap, Options as WrapOptions};

use crate::auth;
use crate::config;
use crate::data::{CommentService, FeedService, InteractionService, SubredditService};
use crate::markdown;
use crate::media;
use crate::reddit;
use crate::release_notes;
use crate::session;
use crate::storage;
use crate::update::{self, SKIP_UPDATE_ENV};
use crate::video::{self, ExternalLaunchOptions, VideoCommand};

const MAX_IMAGE_COLS: i32 = 40;
const MAX_IMAGE_ROWS: i32 = 20;
const MIN_IMAGE_COLS: i32 = 12;
const MIN_IMAGE_ROWS: i32 = 6;
const TARGET_PREVIEW_WIDTH_PX: i64 = 480;
const KITTY_CHUNK_SIZE: usize = 4096;
const MEDIA_INDENT: u16 = 0;
const KITTY_PROBE_TIMEOUT_MS: u64 = 150;
const VIDEO_CACHE_TTL_SECS: u64 = 60 * 60 * 12;
const MAX_PENDING_MEDIA_REQUESTS: usize = 8;

// TODO add richer inline video controls (pause/seek/audio)
const COLOR_BG: Color = Color::Rgb(30, 30, 46);
const COLOR_PANEL_BG: Color = Color::Rgb(24, 24, 36);
const COLOR_PANEL_FOCUSED_BG: Color = Color::Rgb(49, 50, 68);
const COLOR_PANEL_SELECTED_BG: Color = Color::Rgb(69, 71, 90);
const COLOR_BORDER_IDLE: Color = Color::Rgb(49, 50, 68);
const COLOR_BORDER_FOCUSED: Color = Color::Rgb(137, 180, 250);
const COLOR_TEXT_PRIMARY: Color = Color::Rgb(205, 214, 244);
const COLOR_TEXT_SECONDARY: Color = Color::Rgb(166, 173, 200);
const COLOR_ACCENT: Color = Color::Rgb(137, 180, 250);
const COLOR_SUCCESS: Color = Color::Rgb(166, 227, 161);
const COLOR_ERROR: Color = Color::Rgb(243, 139, 168);

const PROJECT_LINK_URL: &str = "https://github.com/ck-zhang/reddix";
const SUPPORT_LINK_URL: &str = "https://ko-fi.com/ckzhang";
const CURRENT_VERSION_OVERRIDE_ENV: &str = "REDDIX_OVERRIDE_CURRENT_VERSION";
const REDDIX_COMMUNITY: &str = "ReddixTUI";
const REDDIX_COMMUNITY_DISPLAY: &str = "r/ReddixTUI";
const MPV_PATH_ENV: &str = "REDDIX_MPV_PATH";
const COMMENT_DEPTH_COLORS: [Color; 6] = [
    Color::Rgb(250, 179, 135),
    Color::Rgb(166, 227, 161),
    Color::Rgb(203, 166, 247),
    Color::Rgb(245, 194, 231),
    Color::Rgb(137, 220, 235),
    Color::Rgb(249, 226, 175),
];

fn comment_depth_color(depth: usize) -> Color {
    COMMENT_DEPTH_COLORS[depth % COMMENT_DEPTH_COLORS.len()]
}

fn vote_from_likes(likes: Option<bool>) -> i32 {
    match likes {
        Some(true) => 1,
        Some(false) => -1,
        None => 0,
    }
}

fn likes_from_vote(vote: i32) -> Option<bool> {
    match vote {
        1 => Some(true),
        -1 => Some(false),
        _ => None,
    }
}

fn toggle_vote_value(old: i32, requested: i32) -> i32 {
    if old == requested {
        0
    } else {
        requested
    }
}

const NAV_SORTS: [reddit::SortOption; 5] = [
    reddit::SortOption::Hot,
    reddit::SortOption::Best,
    reddit::SortOption::New,
    reddit::SortOption::Top,
    reddit::SortOption::Rising,
];
const COMMENT_SORTS: [reddit::CommentSortOption; 6] = [
    reddit::CommentSortOption::Confidence,
    reddit::CommentSortOption::Top,
    reddit::CommentSortOption::New,
    reddit::CommentSortOption::Controversial,
    reddit::CommentSortOption::Old,
    reddit::CommentSortOption::Qa,
];
const FEED_CACHE_TTL: Duration = Duration::from_secs(45);
const COMMENT_CACHE_TTL: Duration = Duration::from_secs(120);
const FEED_CACHE_MAX: usize = 16;
const POST_PRELOAD_THRESHOLD: usize = 5;
const COMMENT_CACHE_MAX: usize = 64;
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const POST_LOADING_HEADER_HEIGHT: usize = 2;
const UPDATE_BANNER_HEIGHT: usize = 2;
const ICON_UPVOTES: &str = "";
const ICON_COMMENTS: &str = "";
const ICON_SUBREDDIT: &str = "";
const ICON_USER: &str = "";

static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("reddix/0.1 (kitty-preview)")
        .build()
        .expect("create http client")
});

#[derive(Clone)]
pub struct PostPreview {
    pub title: String,
    pub body: String,
    pub post: reddit::Post,
    pub links: Vec<LinkEntry>,
}

#[derive(Clone)]
pub struct LinkEntry {
    pub label: String,
    pub url: String,
}

impl LinkEntry {
    fn new<L: Into<String>, U: Into<String>>(label: L, url: U) -> Self {
        Self {
            label: label.into(),
            url: url.into(),
        }
    }
}

#[derive(Clone)]
struct DownloadCandidate {
    url: String,
    suggested_name: String,
}

#[derive(Clone)]
struct MediaSaveJob {
    total: usize,
}

struct MediaSaveOutcome {
    dest_dir: PathBuf,
    saved_paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct NavigationMatch {
    label: String,
    target: NavigationTarget,
    description: Option<String>,
    enabled: bool,
}

#[derive(Clone)]
enum NavigationTarget {
    Subreddit(String),
    User(String),
    Search(String),
}

#[derive(Clone)]
struct NavigationMenuState {
    filter: String,
    matches: Vec<NavigationMatch>,
    editing: bool,
    selected: usize,
}

#[derive(Clone)]
struct ActionMenuEntry {
    label: String,
    action: ActionMenuAction,
    enabled: bool,
}

#[derive(Clone)]
enum ActionMenuMode {
    Root,
    Links,
    Navigation(NavigationMenuState),
}

#[derive(Clone)]
enum ActionMenuAction {
    OpenLinks,
    SaveMedia,
    StartVideo,
    StopVideo,
    OpenVideoExternal,
    OpenNavigation,
    ToggleFullscreen,
    ComposeComment,
}

#[derive(Clone)]
struct HelpSection {
    title: String,
    entries: Vec<(String, String)>,
}

impl HelpSection {
    fn new(title: impl Into<String>, entries: Vec<(impl Into<String>, impl Into<String>)>) -> Self {
        let converted = entries
            .into_iter()
            .map(|(binding, description)| (binding.into(), description.into()))
            .collect();
        Self {
            title: title.into(),
            entries: converted,
        }
    }
}

impl NavigationMatch {
    fn new(label: impl Into<String>, target: NavigationTarget) -> Self {
        Self {
            label: label.into(),
            target,
            description: None,
            enabled: true,
        }
    }

    fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl NavigationMenuState {
    fn new(filter: String, matches: Vec<NavigationMatch>, editing: bool) -> Self {
        let mut state = Self {
            filter,
            matches,
            editing,
            selected: 0,
        };
        state.ensure_selection();
        state
    }

    fn ensure_selection(&mut self) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        if self.selected >= self.matches.len() || !self.matches[self.selected].enabled {
            if let Some(idx) = self.matches.iter().position(|m| m.enabled) {
                self.selected = idx;
            } else {
                self.selected = 0;
            }
        }
    }

    fn active_match(&self) -> Option<&NavigationMatch> {
        self.matches.get(self.selected)
    }

    fn select_next_enabled(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let mut idx = self.selected;
        for _ in 0..self.matches.len() {
            idx = (idx + 1) % self.matches.len();
            if self.matches[idx].enabled {
                self.selected = idx;
                return;
            }
        }
    }

    fn select_previous_enabled(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let mut idx = self.selected;
        for _ in 0..self.matches.len() {
            idx = idx.checked_sub(1).unwrap_or(self.matches.len() - 1);
            if self.matches[idx].enabled {
                self.selected = idx;
                return;
            }
        }
    }

    fn page_next_enabled(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let mut idx = self.selected;
        let len = self.matches.len();
        for _ in 0..len {
            idx = (idx + 5).min(len.saturating_sub(1));
            if self.matches[idx].enabled {
                self.selected = idx;
                return;
            }
            if idx + 1 >= len {
                break;
            }
        }
    }

    fn page_previous_enabled(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let mut idx = self.selected;
        for _ in 0..self.matches.len() {
            if idx == 0 {
                break;
            }
            idx = idx.saturating_sub(5);
            if self.matches[idx].enabled {
                self.selected = idx;
                return;
            }
            if idx == 0 {
                break;
            }
        }
    }
}

impl ActionMenuEntry {
    fn new(label: impl Into<String>, action: ActionMenuAction) -> Self {
        Self {
            label: label.into(),
            action,
            enabled: true,
        }
    }

    fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

#[derive(Clone)]
struct MediaPreview {
    placeholder: Text<'static>,
    kitty: Option<KittyImage>,
    cols: i32,
    rows: i32,
    limited_cols: bool,
    limited_rows: bool,
    video: Option<VideoPreview>,
}

enum MediaLoadOutcome {
    Ready(MediaPreview),
    Absent,
    Deferred,
}

impl MediaPreview {
    fn placeholder(&self) -> &Text<'static> {
        &self.placeholder
    }

    fn kitty_mut(&mut self) -> Option<&mut KittyImage> {
        self.kitty.as_mut()
    }

    fn has_kitty(&self) -> bool {
        self.kitty.is_some()
    }

    fn dims(&self) -> (i32, i32) {
        (self.cols, self.rows)
    }

    fn limited_cols(&self) -> bool {
        self.limited_cols
    }

    fn limited_rows(&self) -> bool {
        self.limited_rows
    }

    fn video(&self) -> Option<&VideoPreview> {
        self.video.as_ref()
    }

    fn has_video(&self) -> bool {
        self.video.is_some()
    }
}

#[derive(Clone)]
struct VideoPreview {
    source: video::VideoSource,
}

#[derive(Clone)]
struct KittyImage {
    id: u32,
    cols: i32,
    rows: i32,
    transmit_chunks: Vec<String>,
    transmitted: bool,
    wrap_tmux: bool,
}

impl KittyImage {
    fn ensure_transmitted<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.transmitted {
            return Ok(());
        }
        for chunk in &self.transmit_chunks {
            writer.write_all(chunk.as_bytes())?;
        }
        writer.flush()?;
        self.transmitted = true;
        Ok(())
    }

    fn placement_sequence(&self) -> String {
        let base = format!(
            "\x1b_Ga=p,q=2,C=1,i={},c={},r={};\x1b\\",
            self.id, self.cols, self.rows
        );
        if self.wrap_tmux {
            format!("\x1bPtmux;\x1b{base}\x1b\\")
        } else {
            base
        }
    }

    fn delete_sequence(&self) -> String {
        Self::delete_sequence_for(self.id, self.wrap_tmux)
    }

    fn delete_sequence_for(id: u32, wrap_tmux: bool) -> String {
        let base = format!("\x1b_Ga=d,q=2,i={id};\x1b\\");
        if wrap_tmux {
            format!("\x1bPtmux;\x1b{}\x1b\\", base)
        } else {
            base
        }
    }
}

#[derive(Clone, Copy, Default)]
struct MediaLayout {
    line_offset: usize,
    indent: u16,
}

#[derive(Clone, Copy)]
struct MediaOrigin {
    row: u16,
    col: u16,
    visual_scroll: usize,
    visual_offset: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct MediaConstraints {
    cols: i32,
    rows: i32,
}

#[derive(Clone, Copy)]
struct CellMetrics {
    width: f64,
    height: f64,
}

fn terminal_cell_metrics() -> CellMetrics {
    static METRICS: OnceLock<CellMetrics> = OnceLock::new();
    *METRICS.get_or_init(|| {
        window_size().ok().map_or(
            CellMetrics {
                width: 1.0,
                height: 1.0,
            },
            |size| {
                let columns = size.columns.max(1) as f64;
                let rows = size.rows.max(1) as f64;
                let width = if size.width > 0 && columns > 0.0 {
                    f64::from(size.width) / columns
                } else {
                    1.0
                };
                let height = if size.height > 0 && rows > 0.0 {
                    f64::from(size.height) / rows
                } else {
                    1.0
                };
                CellMetrics { width, height }
            },
        )
    })
}

fn kitty_debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_truthy("REDDIX_DEBUG_KITTY"))
}

fn kitty_delete_all_sequence() -> String {
    let base = "\x1b_Ga=d,q=0;\x1b\\";
    if tmux_passthrough_enabled() {
        format!("\x1bPtmux;\x1b{}\x1b\\", base)
    } else {
        base.to_string()
    }
}

struct ActiveKitty {
    post_name: String,
    image_id: u32,
    wrap_tmux: bool,
    row: u16,
    col: u16,
}

struct ActiveVideo {
    session: video::InlineSession,
    source: video::VideoSource,
    post_name: String,
    row: u16,
    col: u16,
    cols: i32,
    rows: i32,
}

impl ActiveVideo {
    fn matches_geometry(
        &self,
        row: u16,
        col: u16,
        cols: i32,
        rows: i32,
        source: &video::VideoSource,
    ) -> bool {
        self.row == row
            && self.col == col
            && self.cols == cols
            && self.rows == rows
            && self.source.playback_url == source.playback_url
    }

    fn try_status(&mut self) -> Option<Result<ExitStatus>> {
        self.session.try_status()
    }
}

struct VideoCompletion {
    post_name: String,
    result: Result<ExitStatus>,
    label: String,
    row: u16,
    col: u16,
    cols: i32,
    rows: i32,
}

struct NumericJump {
    value: usize,
    last_input: Instant,
}

fn collect_comments(
    listing: &reddit::Listing<reddit::Comment>,
    depth: usize,
    entries: &mut Vec<CommentEntry>,
) -> usize {
    let mut total = 0;
    for thing in &listing.children {
        if thing.kind == "more" {
            continue;
        }
        let comment = &thing.data;
        if comment.body.trim().is_empty() {
            continue;
        }
        let index = entries.len();
        let (clean_body, found_links) = scrub_links(&comment.body);
        let author_label = if comment.author.trim().is_empty() {
            "[deleted]".to_string()
        } else {
            let author = comment.author.as_str();
            format!("u/{author}")
        };
        let mut link_entries = Vec::new();
        for (idx, url) in found_links.into_iter().enumerate() {
            let number = idx + 1;
            let label = format!("Comment link {number} ({author_label})");
            link_entries.push(LinkEntry::new(label, url));
        }
        entries.push(CommentEntry {
            name: comment.name.clone(),
            author: comment.author.clone(),
            raw_body: comment.body.clone(),
            body: clean_body,
            score: comment.score,
            likes: comment.likes,
            score_hidden: comment.score_hidden,
            depth,
            descendant_count: 0,
            links: link_entries,
            is_post_root: false,
        });
        let child_count = comment
            .replies
            .as_ref()
            .map(|replies| collect_comments(replies, depth + 1, entries))
            .unwrap_or(0);
        entries[index].descendant_count = child_count;
        total += 1 + child_count;
    }
    total
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let percent_x = percent_x.min(100);
    let percent_y = percent_y.min(100);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage(100 - percent_x - (100 - percent_x) / 2),
        ])
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage(100 - percent_y - (100 - percent_y) / 2),
        ])
        .split(horizontal[1]);
    vertical[1]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Navigation,
    Posts,
    Content,
    Comments,
}

impl Pane {
    fn title(self) -> &'static str {
        match self {
            Pane::Navigation => "Navigation",
            Pane::Posts => "Posts",
            Pane::Content => "Content",
            Pane::Comments => "Comments",
        }
    }

    fn next(self) -> Self {
        match self {
            Pane::Navigation => Pane::Posts,
            Pane::Posts => Pane::Content,
            Pane::Content => Pane::Comments,
            Pane::Comments => Pane::Comments,
        }
    }

    fn previous(self) -> Self {
        match self {
            Pane::Navigation => Pane::Navigation,
            Pane::Posts => Pane::Navigation,
            Pane::Content => Pane::Posts,
            Pane::Comments => Pane::Content,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum MenuField {
    #[default]
    ClientId,
    ClientSecret,
    UserAgent,
    Save,
    OpenLink,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuScreen {
    Accounts,
    Credentials,
    ReleaseNotes,
}

#[derive(Clone)]
struct MenuAccountEntry {
    id: i64,
    display: String,
    is_active: bool,
}

#[derive(Default, Clone)]
struct JoinState {
    pending: bool,
    joined: bool,
    last_error: Option<String>,
}

struct MenuAccountPositions {
    add: usize,
    join: usize,
    release_notes: Option<usize>,
    update_check: usize,
    install: Option<usize>,
    github: usize,
    support: usize,
    total: usize,
}

impl JoinState {
    fn mark_pending(&mut self) {
        self.pending = true;
        self.last_error = None;
    }

    fn mark_success(&mut self) {
        self.pending = false;
        self.joined = true;
        self.last_error = None;
    }

    fn mark_error(&mut self, message: String) {
        self.pending = false;
        self.joined = false;
        self.last_error = Some(message);
    }
}

impl MenuField {
    fn next(self, has_link: bool) -> Self {
        match self {
            MenuField::ClientId => MenuField::ClientSecret,
            MenuField::ClientSecret => MenuField::UserAgent,
            MenuField::UserAgent => MenuField::Save,
            MenuField::Save => {
                if has_link {
                    MenuField::OpenLink
                } else {
                    MenuField::ClientId
                }
            }
            MenuField::OpenLink => MenuField::ClientId,
        }
    }

    fn previous(self, has_link: bool) -> Self {
        match self {
            MenuField::ClientId => {
                if has_link {
                    MenuField::OpenLink
                } else {
                    MenuField::Save
                }
            }
            MenuField::ClientSecret => MenuField::ClientId,
            MenuField::UserAgent => MenuField::ClientSecret,
            MenuField::Save => MenuField::UserAgent,
            MenuField::OpenLink => MenuField::Save,
        }
    }

    fn title(self) -> &'static str {
        match self {
            MenuField::ClientId => "Reddit Client ID",
            MenuField::ClientSecret => "Reddit Client Secret",
            MenuField::UserAgent => "User Agent",
            MenuField::Save => "Save & Close",
            MenuField::OpenLink => "Open Authorization Link",
        }
    }
}

#[derive(Default)]
struct MenuForm {
    active: MenuField,
    client_id: String,
    client_secret: String,
    user_agent: String,
    status: Option<String>,
    auth_url: Option<String>,
    auth_pending: bool,
}

impl MenuForm {
    fn reset_status(&mut self) {
        self.status = None;
    }

    fn set_status<S: Into<String>>(&mut self, message: S) {
        self.status = Some(message.into());
    }

    fn focus(&mut self, field: MenuField) {
        if !self.has_auth_link() && matches!(field, MenuField::OpenLink) {
            self.active = MenuField::Save;
        } else {
            self.active = field;
        }
    }

    fn active_accepts_text(&self) -> bool {
        matches!(
            self.active,
            MenuField::ClientId | MenuField::ClientSecret | MenuField::UserAgent
        )
    }

    fn next(&mut self) {
        let has_link = self.has_auth_link();
        self.active = self.active.next(has_link);
    }

    fn previous(&mut self) {
        let has_link = self.has_auth_link();
        self.active = self.active.previous(has_link);
    }

    fn set_values(&mut self, client_id: String, client_secret: String, user_agent: String) {
        self.client_id = client_id;
        self.client_secret = client_secret;
        self.user_agent = user_agent;
    }

    fn active_value_mut(&mut self) -> Option<&mut String> {
        match self.active {
            MenuField::ClientId => Some(&mut self.client_id),
            MenuField::ClientSecret => Some(&mut self.client_secret),
            MenuField::UserAgent => Some(&mut self.user_agent),
            MenuField::Save | MenuField::OpenLink => None,
        }
    }

    fn insert_char(&mut self, ch: char) {
        if let Some(value) = self.active_value_mut() {
            value.push(ch);
        }
        self.reset_status();
    }

    fn backspace(&mut self) {
        if let Some(value) = self.active_value_mut() {
            value.pop();
        }
        self.reset_status();
    }

    fn clear_active(&mut self) {
        if let Some(value) = self.active_value_mut() {
            value.clear();
        }
        self.reset_status();
    }

    fn trimmed_values(&self) -> (String, String, String) {
        (
            self.client_id.trim().to_string(),
            self.client_secret.trim().to_string(),
            self.user_agent.trim().to_string(),
        )
    }

    fn display_value(&self, field: MenuField) -> String {
        let raw = match field {
            MenuField::ClientId => &self.client_id,
            MenuField::ClientSecret => &self.client_secret,
            MenuField::UserAgent => &self.user_agent,
            MenuField::Save | MenuField::OpenLink => return String::new(),
        };
        if raw.is_empty() {
            return "(not set)".to_string();
        }
        if matches!(field, MenuField::ClientSecret) {
            return "*".repeat(raw.chars().count().max(1));
        }
        raw.clone()
    }

    fn authorization_started(&mut self, url: String) {
        self.auth_url = Some(url);
        self.auth_pending = true;
        self.focus(MenuField::OpenLink);
    }

    fn authorization_complete(&mut self) {
        self.auth_pending = false;
        self.auth_url = None;
        if matches!(self.active, MenuField::OpenLink) {
            self.active = MenuField::Save;
        }
    }

    fn has_auth_link(&self) -> bool {
        self.auth_url.is_some()
    }

    fn auth_link(&self) -> Option<&str> {
        self.auth_url.as_deref()
    }
}

#[derive(Clone)]
struct CommentEntry {
    name: String,
    author: String,
    raw_body: String,
    body: String,
    score: i64,
    likes: Option<bool>,
    score_hidden: bool,
    depth: usize,
    descendant_count: usize,
    links: Vec<LinkEntry>,
    is_post_root: bool,
}

#[derive(Clone)]
enum CommentTarget {
    Post {
        post_fullname: String,
        post_title: String,
        subreddit: String,
    },
    Comment {
        post_fullname: String,
        post_title: String,
        comment_fullname: String,
        author: String,
    },
}

impl CommentTarget {
    fn parent_fullname(&self) -> &str {
        match self {
            CommentTarget::Post { post_fullname, .. } => post_fullname,
            CommentTarget::Comment {
                comment_fullname, ..
            } => comment_fullname,
        }
    }

    fn post_fullname(&self) -> &str {
        match self {
            CommentTarget::Post { post_fullname, .. } => post_fullname,
            CommentTarget::Comment { post_fullname, .. } => post_fullname,
        }
    }

    fn description(&self) -> String {
        match self {
            CommentTarget::Post {
                post_title,
                subreddit,
                ..
            } => format!("Replying to {} in {}", post_title, subreddit),
            CommentTarget::Comment { author, .. } => {
                if author.trim().is_empty() {
                    "Replying to a deleted comment".to_string()
                } else {
                    format!("Replying to comment by u/{}", author.trim())
                }
            }
        }
    }

    fn post_title(&self) -> &str {
        match self {
            CommentTarget::Post { post_title, .. } => post_title,
            CommentTarget::Comment { post_title, .. } => post_title,
        }
    }
}

#[derive(Clone)]
struct CommentBuffer {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

impl CommentBuffer {
    fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    fn as_text(&self) -> String {
        self.lines.join("\n")
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines
            .get(row)
            .map(|line| line.chars().count())
            .unwrap_or(0)
    }

    fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len().saturating_sub(1);
        }
        let max_col = self.line_len(self.cursor_row);
        if self.cursor_col > max_col {
            self.cursor_col = max_col;
        }
    }

    fn line_byte_index(line: &str, col: usize) -> usize {
        if col == 0 {
            return 0;
        }
        let mut iter = line.char_indices();
        let mut result = line.len();
        for (idx, (byte_idx, _)) in iter.by_ref().enumerate() {
            if idx == col {
                result = byte_idx;
                break;
            }
        }
        if col >= line.chars().count() {
            line.len()
        } else {
            result
        }
    }

    fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        self.clamp_cursor();
        if let Some(line) = self.lines.get_mut(self.cursor_row) {
            let insert_at = Self::line_byte_index(line, self.cursor_col);
            line.insert(insert_at, ch);
            self.cursor_col += 1;
        }
    }

    fn insert_newline(&mut self) {
        self.clamp_cursor();
        if let Some(line) = self.lines.get_mut(self.cursor_row) {
            let split_at = Self::line_byte_index(line, self.cursor_col);
            let tail = line.split_off(split_at);
            self.lines.insert(self.cursor_row + 1, tail);
            self.cursor_row += 1;
            self.cursor_col = 0;
        } else {
            self.lines.push(String::new());
            self.cursor_row = self.lines.len().saturating_sub(1);
            self.cursor_col = 0;
        }
    }

    fn backspace(&mut self) {
        self.clamp_cursor();
        if self.cursor_row == 0 && self.cursor_col == 0 {
            return;
        }
        if self.cursor_col > 0 {
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                let remove_at = Self::line_byte_index(line, self.cursor_col.saturating_sub(1));
                if remove_at < line.len() {
                    line.remove(remove_at);
                    self.cursor_col = self.cursor_col.saturating_sub(1);
                }
            }
        } else if self.cursor_row > 0 {
            let current_line = self.lines.remove(self.cursor_row);
            self.cursor_row = self.cursor_row.saturating_sub(1);
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                let prev_len = line.chars().count();
                line.push_str(&current_line);
                self.cursor_col = prev_len;
            } else {
                self.lines.insert(0, current_line);
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
        }
        self.clamp_cursor();
    }

    fn delete(&mut self) {
        self.clamp_cursor();
        if let Some(line) = self.lines.get_mut(self.cursor_row) {
            let line_len = line.chars().count();
            if self.cursor_col < line_len {
                let remove_at = Self::line_byte_index(line, self.cursor_col);
                if remove_at < line.len() {
                    line.remove(remove_at);
                }
                return;
            }
        }
        if self.cursor_row + 1 < self.lines.len() {
            let next_line = self.lines.remove(self.cursor_row + 1);
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                line.push_str(&next_line);
            } else {
                self.lines.push(next_line);
            }
        }
        self.clamp_cursor();
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_len(self.cursor_row);
        }
        self.clamp_cursor();
    }

    fn move_right(&mut self) {
        let max_col = self.line_len(self.cursor_row);
        if self.cursor_col < max_col {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.clamp_cursor();
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let max_col = self.line_len(self.cursor_row);
            self.cursor_col = self.cursor_col.min(max_col);
        }
        self.clamp_cursor();
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let max_col = self.line_len(self.cursor_row);
            self.cursor_col = self.cursor_col.min(max_col);
        }
        self.clamp_cursor();
    }

    fn move_home(&mut self) {
        self.cursor_col = 0;
        self.clamp_cursor();
    }

    fn move_end(&mut self) {
        self.cursor_col = self.line_len(self.cursor_row);
    }
}

struct CommentComposer {
    target: CommentTarget,
    buffer: CommentBuffer,
    status: Option<String>,
    submitting: bool,
    scroll_row: usize,
}

impl CommentComposer {
    fn new(target: CommentTarget) -> Self {
        Self {
            target,
            buffer: CommentBuffer::new(),
            status: None,
            submitting: false,
            scroll_row: 0,
        }
    }

    fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    fn set_status<S: Into<String>>(&mut self, message: S) {
        self.status = Some(message.into());
    }

    fn clear_status(&mut self) {
        self.status = None;
    }
}

#[derive(Clone)]
struct PostRowData {
    identity: Vec<Line<'static>>,
    title: Vec<Line<'static>>,
    metrics: Vec<Line<'static>>,
}

#[derive(Clone)]
struct PostRowInput {
    name: String,
    title: String,
    subreddit: String,
    author: String,
    score: i64,
    comments: i64,
    vote: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavMode {
    Sorts,
    Subreddits,
}

struct PendingPosts {
    request_id: u64,
    cancel_flag: Arc<AtomicBool>,
    mode: LoadMode,
}

struct PendingComments {
    request_id: u64,
    post_name: String,
    cancel_flag: Arc<AtomicBool>,
    sort: reddit::CommentSortOption,
}

struct PendingCommentSubmit {
    request_id: u64,
    post_fullname: String,
    target: CommentTarget,
}

struct PendingSubreddits {
    request_id: u64,
}

struct PendingPostRows {
    request_id: u64,
    width: usize,
}

struct PendingContent {
    request_id: u64,
    post_name: String,
    cancel_flag: Arc<AtomicBool>,
}

struct PendingVideo {
    request_id: u64,
    post_name: String,
    source: video::VideoSource,
    origin: MediaOrigin,
    dims: (i32, i32),
    cancel_flag: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadMode {
    Replace,
    Append,
}

#[derive(Clone)]
enum VoteTarget {
    Post { fullname: String },
    Comment { fullname: String },
}

enum AsyncResponse {
    Posts {
        request_id: u64,
        target: String,
        sort: reddit::SortOption,
        result: Result<PostBatch>,
    },
    PostRows {
        request_id: u64,
        width: usize,
        rows: Vec<(String, PostRowData)>,
    },
    Comments {
        request_id: u64,
        post_name: String,
        sort: reddit::CommentSortOption,
        result: Result<Vec<CommentEntry>>,
    },
    Content {
        request_id: u64,
        post_name: String,
        rendered: Text<'static>,
    },
    Subreddits {
        request_id: u64,
        result: Result<Vec<String>>,
    },
    Media {
        post_name: String,
        result: Result<MediaLoadOutcome>,
    },
    InlineVideo {
        request_id: u64,
        post_name: String,
        result: Result<String>,
    },
    ExternalVideo {
        request_id: u64,
        label: String,
        result: Result<()>,
    },
    MediaSave {
        result: Result<MediaSaveOutcome>,
    },
    Login {
        result: Result<String>,
    },
    Update {
        result: Result<Option<update::UpdateInfo>>,
    },
    UpdateInstall {
        result: Result<()>,
    },
    KittyProbe {
        result: Result<bool>,
    },
    JoinStatus {
        account_id: i64,
        result: Result<bool>,
    },
    JoinCommunity {
        account_id: i64,
        result: Result<()>,
    },
    CommentSubmit {
        request_id: u64,
        result: Result<reddit::Comment>,
    },
    VoteResult {
        target: VoteTarget,
        requested: i32,
        previous: i32,
        error: Option<String>,
    },
}

fn comment_lines(
    comment: &CommentEntry,
    width: usize,
    indicator: &str,
    meta_style: Style,
    body_style: Style,
    collapsed: bool,
) -> Vec<Line<'static>> {
    let indent_units = "  ".repeat(comment.depth);
    let indicator_prefix = format!("{indent_units}{indicator} ");
    let spacer = " ".repeat(indicator.chars().count());
    let rest_prefix = format!("{indent_units}{spacer} ");
    let body_prefix = format!("{indent_units}{spacer}  ");

    if comment.is_post_root {
        let mut display = comment.body.clone();
        if display.trim().is_empty() || display.contains("Shift+W") {
            display =
                "Comment section · w starts a new thread · w on a comment replies".to_string();
        }
        let style = meta_style.add_modifier(Modifier::ITALIC);
        return wrap_with_prefixes(
            display.as_str(),
            width,
            indicator_prefix.as_str(),
            rest_prefix.as_str(),
            style,
        );
    }

    let author = if comment.author.trim().is_empty() {
        "[deleted]"
    } else {
        comment.author.as_str()
    };

    let vote_marker = match comment.likes {
        Some(true) => "▲",
        Some(false) => "▼",
        None => "·",
    };

    let mut header = if comment.score_hidden {
        format!("{vote_marker} u/{author} · score hidden")
    } else {
        let score = comment.score;
        format!("{vote_marker} u/{author} · {score} points")
    };
    if collapsed {
        let hidden = comment.descendant_count;
        if hidden > 0 {
            let suffix = if hidden == 1 { "reply" } else { "replies" };
            header.push_str(&format!(" · {hidden} hidden {suffix}"));
        }
    }

    let mut lines = wrap_with_prefixes(
        &header,
        width,
        indicator_prefix.as_str(),
        rest_prefix.as_str(),
        meta_style,
    );

    if comment.body.trim().is_empty() {
        lines.extend(wrap_with_prefix(
            "(no comment body)",
            width,
            body_prefix.as_str(),
            body_style,
        ));
        return lines;
    }

    for raw_line in comment.body.lines() {
        if raw_line.trim().is_empty() {
            lines.push(Line::from(Span::styled(String::new(), body_style)));
            continue;
        }
        lines.extend(wrap_with_prefix(
            raw_line.trim(),
            width,
            body_prefix.as_str(),
            body_style,
        ));
    }

    lines
}

fn is_front_page(name: &str) -> bool {
    let normalized = name.trim().trim_start_matches("r/").trim_start_matches('/');
    normalized.eq_ignore_ascii_case("frontpage")
        || normalized.eq_ignore_ascii_case("home")
        || normalized.is_empty()
}

fn normalize_subreddit_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_slashes = trimmed.trim_start_matches('/');
    let rest = if let Some(stripped) = without_slashes
        .strip_prefix("r/")
        .or_else(|| without_slashes.strip_prefix("R/"))
    {
        stripped.trim_start_matches('/')
    } else {
        without_slashes.trim_start_matches('/')
    };
    let rest = rest.trim();
    if rest.is_empty() {
        "r/frontpage".to_string()
    } else {
        format!("r/{}", rest)
    }
}

fn normalize_user_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_slashes = trimmed.trim_start_matches('/');
    let (core, matched) = if let Some(rest) = without_slashes.strip_prefix("u/") {
        (rest, true)
    } else if let Some(rest) = without_slashes.strip_prefix("U/") {
        (rest, true)
    } else if let Some(rest) = without_slashes.strip_prefix("user/") {
        (rest, true)
    } else if let Some(rest) = without_slashes.strip_prefix("USER/") {
        (rest, true)
    } else if let Some(rest) = without_slashes.strip_prefix("User/") {
        (rest, true)
    } else if let Some(rest) = without_slashes.strip_prefix('@') {
        (rest, true)
    } else {
        (without_slashes, false)
    };
    if !matched {
        return None;
    }
    let normalized = core.trim_start_matches('/').trim();
    if normalized.is_empty() {
        None
    } else {
        Some(format!("u/{}", normalized))
    }
}

fn canonical_search_target(raw: &str) -> Option<String> {
    let query = raw.trim();
    if query.is_empty() {
        None
    } else {
        Some(format!("search: {}", query))
    }
}

fn pop_last_word(text: &mut String) -> bool {
    if text.is_empty() {
        return false;
    }
    let original_len = text.len();
    while text.ends_with(char::is_whitespace) {
        text.pop();
    }
    while let Some(ch) = text.chars().last() {
        if ch.is_whitespace() {
            break;
        }
        text.pop();
    }
    while text.ends_with(char::is_whitespace) {
        text.pop();
    }
    original_len != text.len()
}

fn navigation_display_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("search:") {
        let query = rest.trim();
        if query.is_empty() {
            "Search results".to_string()
        } else {
            format!("Search · {}", query)
        }
    } else {
        trimmed.to_string()
    }
}

fn navigation_target_key(target: &NavigationTarget) -> String {
    match target {
        NavigationTarget::Subreddit(name) => format!("sub:{}", name.to_ascii_lowercase()),
        NavigationTarget::User(name) => format!("user:{}", name.to_ascii_lowercase()),
        NavigationTarget::Search(query) => format!("search:{}", query.to_ascii_lowercase()),
    }
}

fn push_navigation_entry(
    buffer: &mut Vec<NavigationMatch>,
    seen: &mut HashSet<String>,
    entry: NavigationMatch,
) {
    let key = navigation_target_key(&entry.target);
    if seen.insert(key) {
        buffer.push(entry);
    }
}

#[derive(Clone, Copy)]
enum FeedKind<'a> {
    FrontPage,
    Subreddit(&'a str),
    User(&'a str),
    Search(&'a str),
}

fn classify_feed_target(target: &str) -> FeedKind<'_> {
    let trimmed = target.trim();
    if trimmed.is_empty() || is_front_page(trimmed) {
        return FeedKind::FrontPage;
    }

    if let Some(rest) = trimmed.strip_prefix("search:") {
        let query = rest.trim();
        if query.is_empty() {
            FeedKind::FrontPage
        } else {
            FeedKind::Search(query)
        }
    } else if let Some(rest) = trimmed.strip_prefix("u/") {
        let user = rest.trim();
        if user.is_empty() {
            FeedKind::FrontPage
        } else {
            FeedKind::User(user)
        }
    } else if let Some(rest) = trimmed.strip_prefix("U/") {
        let user = rest.trim();
        if user.is_empty() {
            FeedKind::FrontPage
        } else {
            FeedKind::User(user)
        }
    } else {
        let subreddit = trimmed.trim_start_matches("r/").trim_start_matches('/');
        if subreddit.is_empty() {
            FeedKind::FrontPage
        } else {
            FeedKind::Subreddit(subreddit)
        }
    }
}

fn ensure_core_subreddits(subreddits: &mut Vec<String>) {
    let mut combined = vec![
        "r/frontpage".to_string(),
        "r/all".to_string(),
        "r/popular".to_string(),
    ];

    for name in subreddits.drain(..) {
        if !combined
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name.as_str()))
        {
            combined.push(name);
        }
    }

    *subreddits = combined;
}

fn fallback_feed_target(current: &str) -> Option<&'static str> {
    let normalized = normalize_subreddit_name(current);
    if is_front_page(&normalized) {
        Some("r/popular")
    } else if normalized.eq_ignore_ascii_case("r/popular") {
        Some("r/all")
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum CacheScope {
    Anonymous,
    Account(i64),
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct FeedCacheKey {
    target: String,
    sort: reddit::SortOption,
}

impl FeedCacheKey {
    fn new(target: &str, sort: reddit::SortOption) -> Self {
        Self {
            target: target.trim().to_ascii_lowercase(),
            sort,
        }
    }
}

struct FeedCacheEntry {
    batch: PostBatch,
    fetched_at: Instant,
    scope: CacheScope,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct CommentCacheKey {
    post_name: String,
    sort: reddit::CommentSortOption,
}

impl CommentCacheKey {
    fn new(post_name: &str, sort: reddit::CommentSortOption) -> Self {
        Self {
            post_name: post_name.to_string(),
            sort,
        }
    }
}

struct CommentCacheEntry {
    comments: Vec<CommentEntry>,
    fetched_at: Instant,
    scope: CacheScope,
}

struct Spinner {
    index: usize,
    last_tick: Instant,
}

#[derive(Clone)]
struct PostBatch {
    posts: Vec<PostPreview>,
    after: Option<String>,
}

impl Spinner {
    fn new() -> Self {
        Self {
            index: 0,
            last_tick: Instant::now(),
        }
    }

    fn frame(&self) -> &'static str {
        SPINNER_FRAMES[self.index % SPINNER_FRAMES.len()]
    }

    fn advance(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_tick) >= Duration::from_millis(120) {
            self.index = (self.index + 1) % SPINNER_FRAMES.len();
            self.last_tick = now;
            true
        } else {
            false
        }
    }

    fn reset(&mut self) {
        self.index = 0;
        self.last_tick = Instant::now();
    }
}

fn make_preview(post: reddit::Post) -> PostPreview {
    let mut body = String::new();
    let mut links: Vec<LinkEntry> = Vec::new();
    let title = post.title.trim();
    body.push_str(&format!(
        "# {}\n\n",
        if title.is_empty() { "Untitled" } else { title }
    ));

    let trimmed_self = post.selftext.trim();
    if !trimmed_self.is_empty() {
        let (clean_self, found_links) = scrub_links(trimmed_self);
        body.push_str(&clean_self);
        body.push_str("\n\n");
        for (index, url) in found_links.into_iter().enumerate() {
            links.push(LinkEntry::new(format!("Post body link {}", index + 1), url));
        }
    } else {
        let url = post.url.trim();
        if !url.is_empty() {
            links.push(LinkEntry::new("External link", url.to_string()));
        }
        if select_preview_source(&post).is_some() {
            // image preview will render asynchronously; no placeholder text needed
        } else if post.post_hint.eq_ignore_ascii_case("hosted:video")
            || post.post_hint.eq_ignore_ascii_case("rich:video")
        {
            body.push_str("_Inline video preview loading…_\n\n");
        } else {
            body.push_str("_No preview available for this post._\n\n");
        }
    }

    body.push_str("---\n\n");

    let meta_lines: Vec<String> = vec![
        format!("**Subreddit:** {}", post.subreddit),
        format!("**Author:** u/{}", post.author),
        format!("**Score:** {}", post.score),
        format!("**Comments:** {}", post.num_comments),
    ];

    let url = post.url.trim();
    if !url.is_empty() && !links.iter().any(|entry| entry.url == url) {
        links.push(LinkEntry::new("External link", url.to_string()));
    }

    let permalink = post.permalink.trim();
    if !permalink.is_empty() {
        let thread_url = format!("https://reddit.com{}", permalink);
        if !links.iter().any(|entry| entry.url == thread_url) {
            links.push(LinkEntry::new("Reddit thread", thread_url));
        }
    }

    for line in meta_lines {
        body.push_str(&format!("- {}\n", line));
    }

    body.push('\n');

    PostPreview {
        title: post.title.clone(),
        body,
        post,
        links,
    }
}

fn content_from_post(post: &PostPreview) -> String {
    post.body.clone()
}

#[allow(clippy::too_many_arguments)]
fn load_media_preview(
    post: &reddit::Post,
    cancel_flag: &AtomicBool,
    media_handle: Option<media::Handle>,
    max_cols: i32,
    max_rows: i32,
    allow_upscale: bool,
    allow_inline_video: bool,
    priority: media::Priority,
) -> Result<MediaLoadOutcome> {
    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(MediaLoadOutcome::Deferred);
    }

    // TODO gallery support

    let capped_cols = if max_cols > MAX_IMAGE_COLS {
        max_cols.max(1)
    } else {
        max_cols.clamp(1, MAX_IMAGE_COLS)
    };
    let capped_rows = if max_rows > MAX_IMAGE_ROWS {
        max_rows.max(1)
    } else {
        max_rows.clamp(1, MAX_IMAGE_ROWS)
    };

    let video_source = video::find_video_source(post);

    if allow_inline_video {
        if let Some(video_source) = video_source.clone() {
            let width_px = video_source.width.unwrap_or(TARGET_PREVIEW_WIDTH_PX);
            let height_px = video_source.height.unwrap_or(((width_px * 9) / 16).max(1));
            let (cols, rows) =
                clamp_dimensions(width_px, height_px, capped_cols, capped_rows, allow_upscale);
            let limited_cols = cols >= capped_cols && (width_px as i32) > capped_cols;
            let limited_rows = rows >= capped_rows && (height_px as i32) > capped_rows;
            let label = if video_source.label.trim().is_empty() {
                "video"
            } else {
                video_source.label.trim()
            };
            video::debug_log(format!(
                "resolved post={} url={} width={} height={} cols={} rows={}",
                post.name, video_source.playback_url, width_px, height_px, cols, rows
            ));
            let placeholder = kitty_placeholder_text(cols, rows, MEDIA_INDENT, label);
            return Ok(MediaLoadOutcome::Ready(MediaPreview {
                placeholder,
                kitty: None,
                cols,
                rows,
                limited_cols,
                limited_rows,
                video: Some(VideoPreview {
                    source: video_source,
                }),
            }));
        }
    }

    let video_preview = video_source.map(|source| VideoPreview { source });

    let source = match select_preview_source(post) {
        Some(src) => src,
        None => return Ok(MediaLoadOutcome::Absent),
    };

    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(MediaLoadOutcome::Deferred);
    }

    let url = source.url.clone();
    if !is_supported_preview_url(&url) {
        let fallback = indent_media_preview(&format!(
            "[preview omitted: unsupported media — {}]",
            image_label(&url)
        ));
        return Ok(MediaLoadOutcome::Ready(MediaPreview {
            placeholder: text_from_string(fallback),
            kitty: None,
            cols: 0,
            rows: 0,
            limited_cols: false,
            limited_rows: false,
            video: video_preview.clone(),
        }));
    }

    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(MediaLoadOutcome::Deferred);
    }

    let bytes = if let Some(handle) = media_handle {
        match fetch_cached_media_bytes(
            handle,
            &url,
            source.width,
            source.height,
            cancel_flag,
            priority,
        ) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(MediaLoadOutcome::Deferred),
            Err(_) => fetch_image_bytes(&url)
                .with_context(|| format!("download preview image {}", url))?,
        }
    } else {
        fetch_image_bytes(&url).with_context(|| format!("download preview image {}", url))?
    };
    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(MediaLoadOutcome::Deferred);
    }
    if bytes.is_empty() {
        bail!("preview image empty");
    }
    let (cols, rows) = clamp_dimensions(
        source.width,
        source.height,
        capped_cols,
        capped_rows,
        allow_upscale,
    );
    let label = image_label(&url);
    let limited_cols = cols >= capped_cols && (source.width as i32) > capped_cols;
    let limited_rows = rows >= capped_rows && (source.height as i32) > capped_rows;

    if cols < MIN_IMAGE_COLS || rows < MIN_IMAGE_ROWS {
        let fallback = indent_media_preview(&format!(
            "[preview omitted: viewport too small — {}]",
            label
        ));
        return Ok(MediaLoadOutcome::Ready(MediaPreview {
            placeholder: text_from_string(fallback),
            kitty: None,
            cols,
            rows,
            limited_cols,
            limited_rows,
            video: video_preview.clone(),
        }));
    }

    let kitty = kitty_transmit_inline(&bytes, cols, rows, kitty_image_id(&post.name, &url))?;
    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(MediaLoadOutcome::Deferred);
    }
    let placeholder = kitty_placeholder_text(cols, rows, MEDIA_INDENT, &label);
    Ok(MediaLoadOutcome::Ready(MediaPreview {
        placeholder,
        kitty: Some(kitty),
        cols,
        rows,
        limited_cols,
        limited_rows,
        video: video_preview,
    }))
}

fn fetch_cached_media_bytes(
    handle: media::Handle,
    url: &str,
    width: i64,
    height: i64,
    cancel_flag: &AtomicBool,
    priority: media::Priority,
) -> Result<Option<Vec<u8>>> {
    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(None);
    }

    let request = media::Request {
        url: url.to_string(),
        width: (width > 0).then_some(width),
        height: (height > 0).then_some(height),
        priority,
        ..Default::default()
    };

    let rx = handle.enqueue(request);
    let result = rx
        .recv()
        .map_err(|err| anyhow!("media: failed to receive cache result: {}", err))?;

    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(None);
    }

    if result.rejected {
        return Ok(None);
    }

    if let Some(entry) = result.entry {
        let path = Path::new(&entry.file_path);
        let bytes =
            fs::read(path).with_context(|| format!("read cached media {}", path.display()))?;
        Ok(Some(bytes))
    } else if let Some(err) = result.error {
        Err(err)
    } else {
        bail!("media: cache returned empty result")
    }
}

fn fetch_cached_video_path(
    handle: &media::Handle,
    url: &str,
    priority: media::Priority,
) -> Result<PathBuf> {
    if url.trim().is_empty() {
        bail!("media: video url missing");
    }

    let request = media::Request {
        url: url.to_string(),
        media_type: Some("video/mp4".to_string()),
        ttl: Some(Duration::from_secs(VIDEO_CACHE_TTL_SECS)),
        priority,
        ..Default::default()
    };

    let rx = handle.enqueue(request);
    let result = rx
        .recv()
        .map_err(|err| anyhow!("media: failed to receive cache result: {}", err))?;

    if result.rejected {
        bail!("media: cache queue saturated");
    }

    if let Some(entry) = result.entry {
        let path = PathBuf::from(entry.file_path);
        if path.exists() {
            Ok(path)
        } else {
            bail!("media: cached video missing on disk {}", path.display());
        }
    } else if let Some(err) = result.error {
        Err(err)
    } else {
        bail!("media: cache returned empty result")
    }
}

fn kitty_placeholder_text(cols: i32, rows: i32, indent: u16, label: &str) -> Text<'static> {
    let row_count = rows.max(1) as usize;
    let indent_width = indent as usize;
    let indent_str = " ".repeat(indent_width);
    let column_span = " ".repeat(cols.max(1) as usize);
    let row_line = format!("{}{}", indent_str, column_span);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(row_count + 1);
    for _ in 0..row_count {
        lines.push(Line::from(row_line.clone()));
    }
    let label_line = format!("{}[image: {}]", indent_str, label);
    lines.push(Line::from(Span::styled(
        label_line,
        Style::default().fg(COLOR_TEXT_SECONDARY),
    )));
    text_with_lines(lines)
}

fn kitty_image_id(post_name: &str, url: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    post_name.hash(&mut hasher);
    url.hash(&mut hasher);
    (hasher.finish() & 0xFFFF_FFFF) as u32
}

fn select_preview_source(post: &reddit::Post) -> Option<reddit::PreviewSource> {
    post.preview
        .images
        .iter()
        .find(|image| {
            !image.source.url.trim().is_empty()
                || image
                    .resolutions
                    .iter()
                    .any(|res| !res.url.trim().is_empty())
        })
        .and_then(|image| {
            let mut larger: Option<(i64, reddit::PreviewSource)> = None;
            let mut smaller: Option<(i64, reddit::PreviewSource)> = None;

            for candidate in image
                .resolutions
                .iter()
                .chain(std::iter::once(&image.source))
            {
                let sanitized = sanitize_preview_url(&candidate.url);
                if sanitized.is_empty() {
                    continue;
                }
                let mut candidate = candidate.clone();
                candidate.url = sanitized;
                let width = if candidate.width > 0 {
                    candidate.width
                } else {
                    TARGET_PREVIEW_WIDTH_PX
                };

                if width >= TARGET_PREVIEW_WIDTH_PX {
                    let replace = larger
                        .as_ref()
                        .map(|(existing_width, _)| width < *existing_width)
                        .unwrap_or(true);
                    if replace {
                        larger = Some((width, candidate.clone()));
                    }
                } else {
                    let replace = smaller
                        .as_ref()
                        .map(|(existing_width, _)| width > *existing_width)
                        .unwrap_or(true);
                    if replace {
                        smaller = Some((width, candidate.clone()));
                    }
                }
            }

            larger.or(smaller).map(|(_, candidate)| candidate)
        })
}

fn sanitize_preview_url(raw: &str) -> String {
    raw.replace("&amp;", "&")
}

fn is_supported_preview_url(url: &str) -> bool {
    if url.trim().is_empty() {
        return false;
    }

    let lowered = url.to_ascii_lowercase();
    if lowered.contains("format=mp4")
        || lowered.contains("format=gif")
        || lowered.contains("format=gifv")
        || lowered.contains("format=webm")
    {
        return false;
    }

    match Url::parse(url) {
        Ok(parsed) => {
            if let Some(ext) = parsed
                .path()
                .rsplit('.')
                .next()
                .map(|value| value.to_ascii_lowercase())
            {
                match ext.as_str() {
                    "jpg" | "jpeg" | "png" | "webp" | "jpe" => {}
                    "gif" | "gifv" | "mp4" | "webm" | "mkv" => return false,
                    _ => {}
                }
            }

            for (key, value) in parsed.query_pairs() {
                if key.eq_ignore_ascii_case("format") {
                    let value = value.to_ascii_lowercase();
                    if matches!(value.as_str(), "mp4" | "gif" | "gifv" | "webm" | "mkv") {
                        return false;
                    }
                }
            }
            true
        }
        Err(_) => {
            !lowered.ends_with(".mp4")
                && !lowered.ends_with(".gif")
                && !lowered.ends_with(".gifv")
                && !lowered.ends_with(".webm")
                && !lowered.ends_with(".mkv")
        }
    }
}

fn fetch_image_bytes(url: &str) -> Result<Vec<u8>> {
    let response = HTTP_CLIENT
        .get(url)
        .send()
        .with_context(|| format!("request preview {}", url))?;
    if !response.status().is_success() {
        bail!("preview request returned status {}", response.status());
    }
    let mut reader = response;
    let mut bytes = Vec::with_capacity(128 * 1024);
    reader
        .read_to_end(&mut bytes)
        .with_context(|| format!("read preview body {}", url))?;
    Ok(bytes)
}

fn encode_png_for_kitty(bytes: &[u8]) -> Result<Cow<'_, [u8]>> {
    if bytes.is_empty() {
        bail!("preview image had no bytes");
    }

    if matches!(image::guess_format(bytes), Ok(ImageFormat::Png)) {
        return Ok(Cow::Borrowed(bytes));
    }

    let image = image::load_from_memory(bytes).context("decode preview image")?;
    let mut png_bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .context("encode preview as png")?;
    Ok(Cow::Owned(png_bytes))
}

fn tmux_passthrough_enabled() -> bool {
    env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false)
}

fn kitty_transmit_inline(bytes: &[u8], cols: i32, rows: i32, image_id: u32) -> Result<KittyImage> {
    if bytes.is_empty() {
        bail!("no image data provided");
    }

    let png_data = encode_png_for_kitty(bytes)?;

    let cols = cols.max(1);
    let rows = rows.max(1);
    let encoded = general_purpose::STANDARD.encode(png_data.as_ref());
    if encoded.is_empty() {
        bail!("failed to encode image preview");
    }

    let wrap_tmux = tmux_passthrough_enabled();
    let prefix = if wrap_tmux { "\x1bPtmux;\x1b" } else { "" };
    let suffix = if wrap_tmux { "\x1b\\" } else { "" };

    let mut chunks: Vec<String> = Vec::new();
    let mut offset = 0;
    while offset < encoded.len() {
        let end = usize::min(offset + KITTY_CHUNK_SIZE, encoded.len());
        let more = if end < encoded.len() { 1 } else { 0 };
        let mut out = String::new();
        if wrap_tmux {
            out.push_str(prefix);
        }
        if offset == 0 {
            out.push_str(&format!("\x1b_Ga=t,q=2,i={},f=100,m={more};", image_id));
        } else {
            out.push_str(&format!("\x1b_Ga=t,q=2,i={},m={more};", image_id));
        }
        out.push_str(&encoded[offset..end]);
        out.push_str("\x1b\\");
        if wrap_tmux {
            out.push_str(suffix);
        }
        chunks.push(out);
        offset = end;
    }

    Ok(KittyImage {
        id: image_id,
        cols,
        rows,
        transmit_chunks: chunks,
        transmitted: false,
        wrap_tmux,
    })
}

fn indent_media_preview(preview: &str) -> String {
    let text = preview.trim_start_matches('\n').to_string();
    if text.is_empty() {
        return text;
    }
    let indent = " ".repeat(MEDIA_INDENT as usize);
    if indent.is_empty() {
        return text;
    }
    let mut lines: Vec<String> = text.split('\n').map(|line| line.to_string()).collect();
    for line in &mut lines {
        if line.starts_with(&indent) || line.starts_with('\u{1b}') {
            continue;
        }
        if line.is_empty() {
            *line = indent.clone();
        } else {
            line.insert_str(0, indent.as_str());
        }
    }
    lines.join("\n")
}

fn text_from_string(preview: String) -> Text<'static> {
    let lines = preview
        .split('\n')
        .map(|line| Line::from(Span::raw(line.to_string())))
        .collect();
    text_with_lines(lines)
}

fn text_with_lines(lines: Vec<Line<'static>>) -> Text<'static> {
    Text {
        lines,
        alignment: Some(Alignment::Left),
        style: Style::default(),
    }
}

fn wrap_with_prefixes(
    text: &str,
    width: usize,
    first_prefix: &str,
    rest_prefix: &str,
    style: Style,
) -> Vec<Line<'static>> {
    if text.trim().is_empty() {
        return vec![Line::from(Span::styled(String::new(), style))];
    }

    if width == 0 {
        let mut line = String::with_capacity(first_prefix.len() + text.len());
        line.push_str(first_prefix);
        line.push_str(text);
        return vec![Line::from(Span::styled(line, style))];
    }

    let min_width = first_prefix
        .chars()
        .count()
        .max(rest_prefix.chars().count())
        .saturating_add(1);
    let wrap_width = width.max(min_width);
    let options = WrapOptions::new(wrap_width)
        .break_words(false)
        .initial_indent(first_prefix)
        .subsequent_indent(rest_prefix);

    wrap(text, options)
        .into_iter()
        .map(|cow| Line::from(Span::styled(cow.into_owned(), style)))
        .collect()
}

fn wrap_plain(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_with_prefixes(text, width, "", "", style)
}

fn scrub_links(text: &str) -> (String, Vec<String>) {
    static MARKDOWN_LINK_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)\[([^\]]+)\]\((https?://[^\s)]+)\)").expect("valid markdown link regex")
    });
    static BARE_URL_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)https?://[^\s)]+").expect("valid bare url regex"));

    if text.trim().is_empty() {
        return (text.to_string(), Vec::new());
    }

    let mut seen = HashSet::new();
    let mut links: Vec<String> = Vec::new();

    let intermediate = MARKDOWN_LINK_RE
        .replace_all(text, |caps: &Captures| {
            let url = caps[2].to_string();
            if seen.insert(url.clone()) {
                links.push(url);
            }
            caps[1].to_string()
        })
        .to_string();

    let sanitized = BARE_URL_RE
        .replace_all(&intermediate, |caps: &Captures| {
            let url = caps[0].to_string();
            if seen.insert(url.clone()) {
                links.push(url);
            }
            "[link]".to_string()
        })
        .to_string();

    (sanitized, links)
}

fn pad_lines_to_width(lines: &mut [Line<'static>], width: u16) {
    let width = width as usize;
    if width == 0 {
        return;
    }

    for line in lines {
        let mut current_width = 0usize;
        for span in &line.spans {
            current_width =
                current_width.saturating_add(UnicodeWidthStr::width(span.content.as_ref()));
        }
        if current_width >= width {
            continue;
        }
        let pad_style = line.spans.last().map(|span| span.style).unwrap_or_default();
        let padding = " ".repeat(width - current_width);
        line.spans.push(Span::styled(padding, pad_style));
    }
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans.is_empty()
        || line
            .spans
            .iter()
            .all(|span| span.content.as_ref().trim().is_empty())
}

fn line_visual_height(line: &Line<'_>, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    let width = width as usize;
    let content_width: usize = line
        .spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if content_width == 0 {
        1
    } else {
        content_width.div_ceil(width)
    }
}

fn visual_height(lines: &[Line<'_>], width: u16) -> usize {
    lines
        .iter()
        .map(|line| line_visual_height(line, width))
        .sum()
}

fn wrap_with_prefix(text: &str, width: usize, prefix: &str, style: Style) -> Vec<Line<'static>> {
    wrap_with_prefixes(text, width, prefix, prefix, style)
}

fn restyle_lines(template: &[Line<'static>], style: Style) -> Vec<Line<'static>> {
    template
        .iter()
        .map(|line| {
            if line.spans.is_empty() {
                return Line::default();
            }
            let spans: Vec<Span<'static>> = line
                .spans
                .iter()
                .map(|span| Span::styled(span.content.clone(), style))
                .collect();
            Line::from(spans)
        })
        .collect()
}

fn build_post_row_data(
    input: &PostRowInput,
    width: usize,
    score_width: usize,
    comments_width: usize,
) -> PostRowData {
    let identity_line = format!(
        "{ICON_SUBREDDIT} r/{}   {ICON_USER} u/{}",
        input.subreddit, input.author
    );
    let identity = wrap_plain(&identity_line, width, Style::default());

    let title = wrap_plain(&input.title, width, Style::default());

    let vote_marker = match input.vote {
        1 => "▲",
        -1 => "▼",
        _ => " ",
    };
    let metrics_line = format!(
        "{vote_marker} {ICON_UPVOTES} {:>score_width$}   {ICON_COMMENTS} {:>comments_width$}",
        input.score, input.comments
    );
    let metrics = wrap_plain(&metrics_line, width, Style::default());

    PostRowData {
        identity,
        title,
        metrics,
    }
}

fn sort_label(sort: reddit::SortOption) -> &'static str {
    match sort {
        reddit::SortOption::Hot => "/hot",
        reddit::SortOption::Best => "/best",
        reddit::SortOption::New => "/new",
        reddit::SortOption::Top => "/top",
        reddit::SortOption::Rising => "/rising",
    }
}

fn comment_sort_label(sort: reddit::CommentSortOption) -> &'static str {
    match sort {
        reddit::CommentSortOption::Confidence => "/best",
        reddit::CommentSortOption::Top => "/top",
        reddit::CommentSortOption::New => "/new",
        reddit::CommentSortOption::Controversial => "/controversial",
        reddit::CommentSortOption::Old => "/old",
        reddit::CommentSortOption::Qa => "/qa",
    }
}

fn image_label(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .map(|segment| percent_decode_str(segment).decode_utf8_lossy().to_string())
        })
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "media".to_string())
}

fn collect_high_res_media(post: &reddit::Post) -> Vec<DownloadCandidate> {
    let mut candidates = Vec::new();

    if let Some(gallery) = &post.gallery_data {
        if let Some(metadata) = &post.media_metadata {
            for item in &gallery.items {
                if let Some(entry) = metadata.get(&item.media_id) {
                    if entry.status.eq_ignore_ascii_case("failed") {
                        continue;
                    }
                    if let Some(url) = preferred_media_metadata_url(entry) {
                        candidates.push(DownloadCandidate {
                            url: url.clone(),
                            suggested_name: image_label(&url),
                        });
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        if let Some(metadata) = &post.media_metadata {
            for entry in metadata.values() {
                if entry.status.eq_ignore_ascii_case("failed") {
                    continue;
                }
                if let Some(url) = preferred_media_metadata_url(entry) {
                    candidates.push(DownloadCandidate {
                        url: url.clone(),
                        suggested_name: image_label(&url),
                    });
                }
            }
        }
    }

    if candidates.is_empty() {
        if let Some(url) = select_full_image_source(post) {
            candidates.push(DownloadCandidate {
                url: url.clone(),
                suggested_name: image_label(&url),
            });
        }
    }

    if let Some(video_source) = video::find_video_source(post) {
        let url = video_source.playback_url.clone();
        if !url.trim().is_empty() && !candidates.iter().any(|candidate| candidate.url == url) {
            let label = image_label(&url);
            candidates.push(DownloadCandidate {
                url,
                suggested_name: label,
            });
        }
    }

    candidates
}

fn preferred_media_metadata_url(entry: &reddit::MediaMetadata) -> Option<String> {
    let primary = sanitize_preview_url(&entry.full.url);
    if !primary.is_empty() {
        return Some(primary);
    }

    for variant in &entry.preview {
        let candidate = sanitize_preview_url(&variant.url);
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }

    if let Some(gif) = &entry.full.gif {
        let candidate = sanitize_preview_url(gif);
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }

    if let Some(mp4) = &entry.full.mp4 {
        let candidate = sanitize_preview_url(mp4);
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }

    None
}

fn select_full_image_source(post: &reddit::Post) -> Option<String> {
    for image in &post.preview.images {
        let url = sanitize_preview_url(&image.source.url);
        if !url.is_empty() {
            return Some(url);
        }
    }

    let direct = sanitize_preview_url(post.url.trim());
    if !direct.is_empty() && is_supported_preview_url(&direct) {
        return Some(direct);
    }

    None
}

fn safe_file_name(label: &str) -> String {
    let mut sanitized = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            sanitized.push(ch);
        } else if ch.is_whitespace() {
            if !sanitized.ends_with('_') {
                sanitized.push('_');
            }
        } else if !sanitized.ends_with('_') {
            sanitized.push('_');
        }
    }

    let trimmed = sanitized.trim_matches('_').trim_start_matches('.');
    if trimmed.is_empty() {
        "image".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extension_from_content_type(content_type: &str) -> Option<&'static str> {
    let mime = content_type
        .split(';')
        .next()
        .map(|value| value.trim().to_ascii_lowercase())?;
    match mime.as_str() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        _ => None,
    }
}

fn image_format_extension(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("png"),
        ImageFormat::Jpeg => Some("jpg"),
        ImageFormat::Gif => Some("gif"),
        ImageFormat::WebP => Some("webp"),
        ImageFormat::Bmp => Some("bmp"),
        ImageFormat::Tiff => Some("tiff"),
        _ => None,
    }
}

fn default_download_dir() -> PathBuf {
    if let Some(dir) = dirs::download_dir() {
        return dir;
    }
    if let Some(home) = dirs::home_dir() {
        return home.join("Downloads");
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn ensure_unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let mut candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("image");
    let ext = path.extension().and_then(|value| value.to_str());

    for index in 2..10_000 {
        let candidate_name = match ext {
            Some(ext) => format!("{stem}-{index}.{ext}"),
            None => format!("{stem}-{index}"),
        };
        candidate = dir.join(&candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    dir.join(format!("{stem}-{}.tmp", Utc::now().timestamp()))
}

fn download_high_res_media(url: &str, suggested_name: &str, dest_dir: &Path) -> Result<PathBuf> {
    let mut response = HTTP_CLIENT
        .get(url)
        .send()
        .with_context(|| format!("request full image {}", url))?;

    if !response.status().is_success() {
        bail!("download failed with status {}", response.status());
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut bytes = Vec::with_capacity(256 * 1024);
    response
        .read_to_end(&mut bytes)
        .with_context(|| format!("read full image {}", url))?;

    if bytes.is_empty() {
        bail!("download returned no data");
    }

    let mut file_name = safe_file_name(suggested_name);
    if Path::new(&file_name).extension().is_none() {
        if let Some(ext) = content_type
            .as_deref()
            .and_then(extension_from_content_type)
        {
            file_name = format!("{file_name}.{ext}");
        }
    }

    if Path::new(&file_name).extension().is_none() {
        if let Ok(format) = image::guess_format(&bytes) {
            if let Some(ext) = image_format_extension(format) {
                file_name = format!("{file_name}.{ext}");
            }
        }
    }

    if Path::new(&file_name).extension().is_none() {
        file_name.push_str(".img");
    }

    let path = ensure_unique_path(dest_dir, &file_name);
    fs::write(&path, &bytes).with_context(|| format!("write image {}", path.display()))?;
    Ok(path)
}

fn save_media_batch(
    candidates: Vec<DownloadCandidate>,
    dest_dir: PathBuf,
) -> Result<MediaSaveOutcome> {
    if !dest_dir.exists() {
        fs::create_dir_all(&dest_dir)
            .with_context(|| format!("prepare download directory {}", dest_dir.display()))?;
    }

    let mut saved_paths = Vec::new();
    for candidate in candidates {
        let path = download_high_res_media(&candidate.url, &candidate.suggested_name, &dest_dir)?;
        saved_paths.push(path);
    }

    Ok(MediaSaveOutcome {
        dest_dir,
        saved_paths,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KittyStatus {
    Unknown,
    Supported,
    Unsupported,
    ForcedEnabled,
    ForcedDisabled,
}

impl KittyStatus {
    fn is_enabled(self) -> bool {
        matches!(self, KittyStatus::Supported | KittyStatus::ForcedEnabled)
    }

    fn is_forced(self) -> bool {
        matches!(
            self,
            KittyStatus::ForcedEnabled | KittyStatus::ForcedDisabled
        )
    }
}

static KITTY_PROBE_STARTED: AtomicBool = AtomicBool::new(false);

fn determine_initial_kitty_status() -> KittyStatus {
    if env_truthy("REDDIX_DISABLE_KITTY") {
        return KittyStatus::ForcedDisabled;
    }
    if env_truthy("REDDIX_FORCE_KITTY") {
        return KittyStatus::ForcedEnabled;
    }
    let enable_override = env_truthy("REDDIX_ENABLE_KITTY");
    if running_inside_tmux() && !enable_override {
        return KittyStatus::Unsupported;
    }
    if enable_override {
        return KittyStatus::ForcedEnabled;
    }
    if env::var("KITTY_WINDOW_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return KittyStatus::Supported;
    }
    if terminal_hints_kitty_support() {
        return KittyStatus::Supported;
    }
    KittyStatus::Unknown
}

fn terminal_hints_kitty_support() -> bool {
    fn matches_known(value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        const KEYWORDS: [&str; 6] = ["kitty", "wezterm", "ghostty", "konsole", "warp", "wayst"];
        if KEYWORDS.iter().any(|kw| lower.contains(kw)) {
            return true;
        }
        lower == "st" || lower.starts_with("st-")
    }

    env::var("TERM")
        .ok()
        .filter(|term| matches_known(term))
        .is_some()
        || env::var("TERM_PROGRAM")
            .ok()
            .filter(|program| matches_known(program))
            .is_some()
}

fn env_truthy(key: &str) -> bool {
    env::var(key)
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True" | "yes" | "YES"))
        .unwrap_or(false)
}

fn running_inside_tmux() -> bool {
    let in_tmux = env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false)
        || env::var("TMUX_PANE")
            .map(|v| !v.is_empty())
            .unwrap_or(false);

    if in_tmux {
        return true;
    }

    env::var("TERM")
        .map(|term| term.to_ascii_lowercase().contains("tmux"))
        .unwrap_or(false)
}

fn detect_kitty_graphics() -> Result<bool> {
    probe_kitty_graphics(Duration::from_millis(KITTY_PROBE_TIMEOUT_MS))
}

#[cfg(unix)]
struct TtyModeGuard {
    fd: RawFd,
    original_flags: i32,
    original_termios: libc::termios,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventKind {
    KittyResponse,
    DeviceAttributes,
    OtherEscape,
}

#[cfg(unix)]
impl TtyModeGuard {
    fn new(fd: RawFd) -> Result<Self> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        if flags < 0 {
            return Err(io::Error::last_os_error()).context("read tty flags");
        }

        let mut termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
            return Err(io::Error::last_os_error()).context("read tty attributes");
        }

        Ok(Self {
            fd,
            original_flags: flags,
            original_termios: termios,
        })
    }

    fn enter_raw_nonblocking(&self) -> Result<()> {
        let mut raw = self.original_termios;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_iflag &= !(libc::IXON | libc::IXOFF);
        raw.c_oflag &= !libc::OPOST;
        #[allow(clippy::unnecessary_cast)]
        {
            raw.c_cc[libc::VMIN as usize] = 0;
            raw.c_cc[libc::VTIME as usize] = 0;
        }

        if unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error()).context("set raw tty mode");
        }

        if unsafe {
            libc::fcntl(
                self.fd,
                libc::F_SETFL,
                self.original_flags | libc::O_NONBLOCK,
            )
        } != 0
        {
            return Err(io::Error::last_os_error()).context("set nonblocking tty mode");
        }

        Ok(())
    }
}

#[cfg(unix)]
impl Drop for TtyModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original_termios);
            libc::fcntl(self.fd, libc::F_SETFL, self.original_flags);
        }
    }
}

#[cfg(unix)]
fn extract_event(buffer: &mut Vec<u8>) -> Option<EventKind> {
    let mut idx = 0;
    while idx + 1 < buffer.len() {
        if buffer[idx] != 0x1b {
            idx += 1;
            continue;
        }
        let next = buffer[idx + 1];
        match next {
            b'G' => {
                if let Some(end) = find_esc_backslash(&buffer[idx + 2..]) {
                    let end_idx = idx + 2 + end;
                    buffer.drain(idx..=end_idx + 1);
                    return Some(EventKind::KittyResponse);
                }
                break;
            }
            b'[' => {
                if let Some(end_idx) = find_csi_terminator(&buffer[idx + 2..]) {
                    let absolute = idx + 2 + end_idx;
                    buffer.drain(idx..=absolute);
                    return Some(EventKind::DeviceAttributes);
                }
                break;
            }
            _ => {
                if let Some(end_idx) = find_escape_terminated(&buffer[idx + 1..]) {
                    let absolute = idx + 1 + end_idx;
                    buffer.drain(idx..=absolute);
                    return Some(EventKind::OtherEscape);
                }
                break;
            }
        }
    }
    None
}

#[cfg(unix)]
fn find_esc_backslash(slice: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < slice.len() {
        if slice[i] == 0x1b && slice[i + 1] == b'\\' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(unix)]
fn find_csi_terminator(slice: &[u8]) -> Option<usize> {
    for (offset, byte) in slice.iter().enumerate() {
        if (0x40..=0x7e).contains(byte) {
            return Some(offset);
        }
    }
    None
}

#[cfg(unix)]
fn find_escape_terminated(slice: &[u8]) -> Option<usize> {
    for (offset, byte) in slice.iter().enumerate() {
        if (0x40..=0x7e).contains(byte) {
            return Some(offset);
        }
    }
    None
}

#[cfg(unix)]
fn probe_kitty_graphics(timeout: Duration) -> Result<bool> {
    let mut tty = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(tty) => tty,
        Err(err) => return Err(err).context("open /dev/tty for kitty probe"),
    };

    let fd = tty.as_raw_fd();
    let guard = TtyModeGuard::new(fd)?;
    guard.enter_raw_nonblocking()?;

    let query = b"\x1b_Gi=1,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
    tty.write_all(query)
        .context("write kitty query escape sequence")?;
    let _ = tty.flush();

    let deadline = Instant::now() + timeout;
    let mut buffer: Vec<u8> = Vec::new();
    let mut first_event: Option<EventKind> = None;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = remaining
            .as_millis()
            .min(i32::MAX as u128)
            .try_into()
            .unwrap_or(i32::MAX);

        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let poll_result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if poll_result < 0 {
            return Err(io::Error::last_os_error()).context("poll for kitty probe response");
        }
        if poll_result == 0 {
            continue;
        }
        if (pollfd.revents & libc::POLLIN) == 0 {
            continue;
        }

        let mut chunk = [0u8; 512];
        loop {
            match tty.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context("read kitty probe response"),
            }
        }

        while let Some(event) = extract_event(&mut buffer) {
            if first_event.is_none()
                && matches!(
                    event,
                    EventKind::KittyResponse | EventKind::DeviceAttributes
                )
            {
                first_event = Some(event);
            }
            if matches!(event, EventKind::KittyResponse) {
                drop(guard);
                return Ok(true);
            }
        }
    }

    drop(guard);
    Ok(matches!(first_event, Some(EventKind::KittyResponse)))
}

#[cfg(not(unix))]
fn probe_kitty_graphics(_timeout: Duration) -> Result<bool> {
    Ok(false)
}

fn clamp_dimensions(
    width: i64,
    height: i64,
    max_width: i32,
    max_height: i32,
    allow_upscale: bool,
) -> (i32, i32) {
    let metrics = terminal_cell_metrics();
    let cell_width = metrics.width.max(1.0);
    let cell_height = metrics.height.max(1.0);

    let width_px = if width <= 0 {
        TARGET_PREVIEW_WIDTH_PX as f64
    } else {
        width as f64
    };
    let height_px = if height <= 0 {
        TARGET_PREVIEW_WIDTH_PX as f64
    } else {
        height as f64
    };

    let mut native_cols = width_px / cell_width;
    let mut native_rows = height_px / cell_height;
    if native_cols <= 0.0 {
        native_cols = max_width.max(1) as f64;
    }
    if native_rows <= 0.0 {
        native_rows = max_height.max(1) as f64;
    }

    let max_cols = max_width.max(1);
    let max_rows = max_height.max(1);
    let max_width_cells = max_cols as f64;
    let max_height_cells = max_rows as f64;
    let scale_x = max_width_cells / native_cols;
    let scale_y = max_height_cells / native_rows;
    let mut scale = scale_x.min(scale_y);
    if !allow_upscale && scale > 1.0 {
        scale = 1.0;
    }
    if scale <= 0.0 {
        scale = 1.0;
    }

    let cols = (native_cols * scale).round() as i32;
    let rows = (native_rows * scale).round() as i32;
    (cols.clamp(1, max_cols), rows.clamp(1, max_rows))
}

fn copy_to_clipboard(state: &mut Option<Clipboard>, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }

    if state.is_none() {
        *state = Some(Clipboard::new().context("open system clipboard")?);
    }

    if let Some(clipboard) = state.as_mut() {
        let payload = text.to_string();
        match clipboard.set_text(payload.clone()) {
            Ok(()) => return Ok(()),
            Err(_err) => {
                *state = None;
                let mut fresh = Clipboard::new().context("reopen system clipboard")?;
                fresh.set_text(payload).context("write text to clipboard")?;
                *state = Some(fresh);
                return Ok(());
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
pub struct Options {
    pub status_message: String,
    pub subreddits: Vec<String>,
    pub posts: Vec<PostPreview>,
    pub content: String,
    pub feed_service: Option<Arc<dyn FeedService + Send + Sync>>,
    pub subreddit_service: Option<Arc<dyn SubredditService + Send + Sync>>,
    pub default_sort: reddit::SortOption,
    pub default_comment_sort: reddit::CommentSortOption,
    pub comment_service: Option<Arc<dyn CommentService + Send + Sync>>,
    pub interaction_service: Option<Arc<dyn InteractionService + Send + Sync>>,
    pub media_handle: Option<media::Handle>,
    pub config_path: String,
    pub store: Arc<storage::Store>,
    pub session_manager: Option<Arc<session::Manager>>,
    pub fetch_subreddits_on_start: bool,
}

pub struct Model {
    status_message: String,
    subreddits: Vec<String>,
    posts: Vec<PostPreview>,
    feed_after: Option<String>,
    comments: Vec<CommentEntry>,
    visible_comment_indices: Vec<usize>,
    collapsed_comments: HashSet<usize>,
    content: Text<'static>,
    fallback_content: Text<'static>,
    fallback_source: String,
    content_source: String,
    media_previews: HashMap<String, MediaPreview>,
    media_failures: HashSet<String>,
    pending_media: HashMap<String, Arc<AtomicBool>>,
    pending_media_order: VecDeque<String>,
    pending_video: Option<PendingVideo>,
    pending_video_clear: Option<(u16, u16, i32, i32)>,
    pending_external_video: Option<u64>,
    media_save_in_progress: Option<MediaSaveJob>,
    media_layouts: HashMap<String, MediaLayout>,
    media_handle: Option<media::Handle>,
    media_constraints: MediaConstraints,
    video_completed_post: Option<String>,
    terminal_cols: u16,
    terminal_rows: u16,
    media_fullscreen: bool,
    media_fullscreen_prev_focus: Option<Pane>,
    media_fullscreen_prev_scroll: Option<u16>,
    feed_cache: HashMap<FeedCacheKey, FeedCacheEntry>,
    comment_cache: HashMap<CommentCacheKey, CommentCacheEntry>,
    post_rows: HashMap<String, PostRowData>,
    post_rows_width: usize,
    pending_post_rows: Option<PendingPostRows>,
    content_cache: HashMap<String, Text<'static>>,
    pending_content: Option<PendingContent>,
    cache_scope: CacheScope,
    selected_sub: usize,
    selected_post: usize,
    selected_comment: usize,
    post_offset: Cell<usize>,
    post_view_height: Cell<u16>,
    comment_offset: Cell<usize>,
    comment_view_height: Cell<u16>,
    comment_view_width: Cell<u16>,
    comment_status_height: Cell<usize>,
    subreddit_offset: Cell<usize>,
    subreddit_view_height: Cell<u16>,
    subreddit_view_width: Cell<u16>,
    nav_index: usize,
    nav_mode: NavMode,
    content_scroll: u16,
    content_area: Option<Rect>,
    needs_kitty_flush: bool,
    pending_kitty_deletes: Vec<String>,
    active_kitty: Option<ActiveKitty>,
    interaction_service: Option<Arc<dyn InteractionService + Send + Sync>>,
    feed_service: Option<Arc<dyn FeedService + Send + Sync>>,
    subreddit_service: Option<Arc<dyn SubredditService + Send + Sync>>,
    comment_service: Option<Arc<dyn CommentService + Send + Sync>>,
    sort: reddit::SortOption,
    comment_sort: reddit::CommentSortOption,
    comment_sort_selected: bool,
    focused_pane: Pane,
    menu_visible: bool,
    menu_screen: MenuScreen,
    menu_form: MenuForm,
    menu_accounts: Vec<MenuAccountEntry>,
    menu_account_index: usize,
    action_menu_visible: bool,
    help_visible: bool,
    action_menu_mode: ActionMenuMode,
    action_menu_items: Vec<ActionMenuEntry>,
    action_menu_selected: usize,
    action_link_items: Vec<LinkEntry>,
    join_states: HashMap<i64, JoinState>,
    update_notice: Option<update::UpdateInfo>,
    update_check_in_progress: bool,
    update_checked: bool,
    update_install_in_progress: bool,
    update_banner_selected: bool,
    update_install_finished: bool,
    release_note_active: bool,
    release_note: Option<release_notes::ReleaseNote>,
    release_note_unread: bool,
    latest_known_version: Option<Version>,
    current_version: Version,
    store: Arc<storage::Store>,
    session_manager: Option<Arc<session::Manager>>,
    login_in_progress: bool,
    needs_redraw: bool,
    numeric_jump: Option<NumericJump>,
    spinner: Spinner,
    config_path: String,
    comment_status: String,
    comment_composer: Option<CommentComposer>,
    response_tx: Sender<AsyncResponse>,
    response_rx: Receiver<AsyncResponse>,
    next_request_id: u64,
    pending_posts: Option<PendingPosts>,
    pending_comments: Option<PendingComments>,
    pending_comment_submit: Option<PendingCommentSubmit>,
    pending_subreddits: Option<PendingSubreddits>,
    needs_video_refresh: bool,
    active_video: Option<ActiveVideo>,
    clipboard: Option<Clipboard>,
    kitty_status: KittyStatus,
    kitty_probe_in_progress: bool,
    show_nsfw: bool,
}

impl Model {
    fn queue_update_check(&mut self) {
        if self.update_checked || self.update_check_in_progress {
            return;
        }
        if env::var(SKIP_UPDATE_ENV).is_ok() {
            self.update_checked = true;
            self.update_notice = None;
            self.update_check_in_progress = false;
            self.latest_known_version = Some(self.current_version.clone());
            self.status_message = format!("Update check skipped: {SKIP_UPDATE_ENV} is set.");
            self.mark_dirty();
            return;
        }
        if cfg!(test) {
            self.update_checked = true;
            self.latest_known_version = Some(self.current_version.clone());
            return;
        }
        self.update_checked = true;
        self.update_check_in_progress = true;
        self.latest_known_version = None;
        self.update_install_finished = false;
        self.mark_dirty();
        let tx = self.response_tx.clone();
        let version = self.current_version.clone();
        thread::spawn(move || {
            let result = update::check_for_update(&version);
            let _ = tx.send(AsyncResponse::Update { result });
        });
    }

    fn force_update_check(&mut self) {
        self.update_check_in_progress = false;
        self.update_checked = false;
        self.update_notice = None;
        self.update_banner_selected = false;
        self.latest_known_version = None;
        self.update_install_finished = false;
        self.status_message = "Checking for updates…".to_string();
        self.mark_dirty();
        self.queue_update_check();
    }

    fn has_update_banner(&self) -> bool {
        self.update_notice.is_some()
    }

    fn initialize_kitty_detection(&mut self) {
        self.kitty_probe_in_progress = false;
        if cfg!(test) {
            self.apply_kitty_status(KittyStatus::Unsupported);
            return;
        }
        let status = determine_initial_kitty_status();
        self.apply_kitty_status(status);
        self.queue_kitty_detection_if_needed();
    }

    fn queue_kitty_detection_if_needed(&mut self) {
        if self.kitty_status != KittyStatus::Unknown || self.kitty_probe_in_progress {
            return;
        }
        if !env_truthy("REDDIX_EXPERIMENTAL_KITTY_PROBE") {
            return;
        }
        if KITTY_PROBE_STARTED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.kitty_probe_in_progress = true;
        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let result = detect_kitty_graphics();
            let _ = tx.send(AsyncResponse::KittyProbe { result });
        });
    }

    fn handle_kitty_probe(&mut self, result: Result<bool>) {
        self.kitty_probe_in_progress = false;
        if self.kitty_status.is_forced() {
            return;
        }
        match result {
            Ok(true) => {
                if self.kitty_status == KittyStatus::Unknown {
                    self.apply_kitty_status(KittyStatus::Supported);
                    self.status_message =
                        "Kitty graphics detected. Inline previews enabled.".to_string();
                }
            }
            Ok(false) => {
                if self.kitty_status == KittyStatus::Unknown {
                    self.apply_kitty_status(KittyStatus::Unsupported);
                }
            }
            Err(err) => {
                if self.kitty_status == KittyStatus::Unknown {
                    self.apply_kitty_status(KittyStatus::Unsupported);
                }
                self.status_message = format!("Kitty graphics detection failed: {}", err);
            }
        }
        self.mark_dirty();
    }

    fn apply_kitty_status(&mut self, status: KittyStatus) {
        if self.kitty_status == status {
            return;
        }
        self.kitty_status = status;
        if let Some(post) = self.posts.get(self.selected_post).cloned() {
            let key = post.post.name.clone();
            if let Some(cancel) = self.pending_media.remove(&key) {
                cancel.store(true, Ordering::SeqCst);
            }
            self.remove_pending_media_tracking(&key);
            self.media_failures.remove(&key);
            self.media_previews.remove(&key);
            self.ensure_media_request_ready(&post);
        }
        if !status.is_enabled() {
            self.queue_active_kitty_delete();
        }
        self.mark_dirty();
    }

    fn banner_selected(&self) -> bool {
        self.has_update_banner() && self.update_banner_selected
    }

    fn post_list_len(&self) -> usize {
        self.posts
            .len()
            .saturating_add(if self.has_update_banner() { 1 } else { 0 })
    }

    fn current_post_cursor(&self) -> usize {
        if self.banner_selected() {
            0
        } else if self.has_update_banner() {
            self.selected_post.saturating_add(1)
        } else {
            self.selected_post
        }
    }

    fn set_post_cursor(&mut self, cursor: usize) {
        if self.has_update_banner() {
            if cursor == 0 {
                if !self.update_banner_selected {
                    self.update_banner_selected = true;
                    self.post_offset.set(0);
                    self.close_action_menu(None);
                    if let Some(update) = &self.update_notice {
                        if self.update_install_finished {
                            self.status_message = format!(
                                "Update v{} installed. Restart Reddix to use the new version.",
                                update.version
                            );
                        } else if self.update_install_in_progress {
                            self.status_message = format!(
                                "Installing update v{}… you can keep browsing while it finishes.",
                                update.version
                            );
                        } else {
                            self.status_message = format!(
                                "Update {} available — press Enter or Shift+U to install now.",
                                update.version
                            );
                        }
                    } else {
                        self.status_message =
                            "Update status unavailable. Try re-running the check.".to_string();
                    }
                    self.mark_dirty();
                }
                return;
            }
            self.update_banner_selected = false;
            let idx = cursor.saturating_sub(1);
            self.select_post_at(idx);
        } else {
            self.update_banner_selected = false;
            self.select_post_at(cursor);
        }
    }

    fn install_update(&mut self) -> Result<()> {
        if self.update_install_in_progress {
            self.status_message =
                "Update install already in progress. Hang tight for completion.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let Some(info) = self.update_notice.clone() else {
            self.status_message =
                "No update available. Trigger a fresh check to look for new releases.".to_string();
            self.mark_dirty();
            return Ok(());
        };

        self.update_install_in_progress = true;
        self.update_install_finished = false;
        self.status_message = format!("Installing update v{}…", info.version);
        self.mark_dirty();

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let result = update::install_update(&info);
            let _ = tx.send(AsyncResponse::UpdateInstall { result });
        });

        Ok(())
    }

    fn version_summary(&self) -> String {
        if self.update_check_in_progress {
            return format!("v{} (checking updates…)", self.current_version);
        }
        if self.update_install_in_progress {
            if let Some(update) = &self.update_notice {
                return format!(
                    "v{} → v{} installing…",
                    self.current_version, update.version
                );
            }
            return format!("v{} (installing update…)", self.current_version);
        }
        if let Some(update) = &self.update_notice {
            if update.version > self.current_version {
                return format!("v{} → v{} available", self.current_version, update.version);
            }
        }
        if let Some(latest) = &self.latest_known_version {
            if latest > &self.current_version {
                return format!("v{} → v{} available", self.current_version, latest);
            }
            return format!("v{} (latest)", self.current_version);
        }
        if self.update_checked {
            return format!("v{} (latest status unknown)", self.current_version);
        }
        format!("v{} (update check pending)", self.current_version)
    }

    fn active_account_id(&self) -> Option<i64> {
        self.session_manager
            .as_ref()
            .and_then(|manager| manager.active_account_id())
    }

    fn active_join_state(&self) -> Option<&JoinState> {
        let account_id = self.active_account_id()?;
        self.join_states.get(&account_id)
    }

    fn menu_account_positions(&self) -> MenuAccountPositions {
        let mut next = self.menu_accounts.len();
        let add = next;
        next += 1;
        let join = next;
        next += 1;
        let release_notes = if self.release_note.is_some() {
            let idx = next;
            next += 1;
            Some(idx)
        } else {
            None
        };
        let update_check = next;
        next += 1;
        let install = if self.update_notice.is_some() {
            let idx = next;
            next += 1;
            Some(idx)
        } else {
            None
        };
        let github = next;
        next += 1;
        let support = next;
        next += 1;
        let total = next;
        MenuAccountPositions {
            add,
            join,
            release_notes,
            update_check,
            install,
            github,
            support,
            total,
        }
    }

    fn queue_join_status_check(&mut self) {
        let Some(service) = self.interaction_service.clone() else {
            return;
        };
        let Some(account_id) = self.active_account_id() else {
            return;
        };
        self.join_states.entry(account_id).or_default();
        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let result = service.is_subscribed(REDDIX_COMMUNITY);
            let _ = tx.send(AsyncResponse::JoinStatus { account_id, result });
        });
    }

    fn join_reddix_subreddit(&mut self) -> Result<()> {
        let Some(service) = self.interaction_service.clone() else {
            self.status_message = format!("Sign in to join {}.", REDDIX_COMMUNITY_DISPLAY);
            self.mark_dirty();
            return Ok(());
        };

        let Some(account_id) = self.active_account_id() else {
            self.status_message = "Select an account before joining the community.".to_string();
            self.mark_dirty();
            return Ok(());
        };

        let state = self.join_states.entry(account_id).or_default();
        if state.joined {
            self.status_message = format!("Already subscribed to {}.", REDDIX_COMMUNITY_DISPLAY);
            self.mark_dirty();
            return Ok(());
        }
        if state.pending {
            self.status_message = format!(
                "Joining {} is already in progress...",
                REDDIX_COMMUNITY_DISPLAY
            );
            self.mark_dirty();
            return Ok(());
        }

        state.mark_pending();
        self.status_message = format!("Joining {}…", REDDIX_COMMUNITY_DISPLAY);
        self.mark_dirty();

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let result = service.subscribe(REDDIX_COMMUNITY);
            let _ = tx.send(AsyncResponse::JoinCommunity { account_id, result });
        });

        Ok(())
    }

    fn show_release_notes_screen(&mut self) -> Result<()> {
        let Some(note) = self.release_note.clone() else {
            self.status_message = "No release notes available right now.".to_string();
            self.mark_dirty();
            return Ok(());
        };
        self.release_note_unread = false;
        self.show_release_note_in_content(&note);
        self.menu_screen = MenuScreen::ReleaseNotes;
        self.status_message = format!(
            "Release notes for v{} — Enter/o opens browser · Esc returns to accounts.",
            note.version
        );
        self.mark_dirty();
        Ok(())
    }

    fn account_display_name(account: &storage::Account) -> String {
        if !account.display_name.trim().is_empty() {
            account.display_name.trim().to_string()
        } else if !account.username.trim().is_empty() {
            account.username.trim().to_string()
        } else {
            account.reddit_id.trim().to_string()
        }
    }

    fn refresh_menu_accounts(&mut self) -> Result<()> {
        let accounts = self
            .store
            .list_accounts()
            .context("list saved Reddit accounts")?;
        let active_id = self
            .session_manager
            .as_ref()
            .and_then(|manager| manager.active_account_id());
        self.menu_accounts = accounts
            .into_iter()
            .map(|account| MenuAccountEntry {
                id: account.id,
                display: Self::account_display_name(&account),
                is_active: active_id == Some(account.id),
            })
            .collect();
        let positions = self.menu_account_positions();
        if positions.total == 0 {
            self.menu_account_index = 0;
        } else {
            self.menu_account_index = self
                .menu_account_index
                .min(positions.total.saturating_sub(1));
        }
        Ok(())
    }

    fn current_cache_scope(&self) -> CacheScope {
        self.session_manager
            .as_ref()
            .and_then(|manager| manager.active_account_id())
            .map(CacheScope::Account)
            .unwrap_or(CacheScope::Anonymous)
    }

    fn ensure_cache_scope(&mut self) {
        let scope = self.current_cache_scope();
        self.adopt_cache_scope(scope);
    }

    fn adopt_cache_scope(&mut self, scope: CacheScope) {
        if self.cache_scope == scope {
            return;
        }
        self.cache_scope = scope;
        self.reset_scoped_caches();
    }

    fn reset_scoped_caches(&mut self) {
        if let Some(pending) = self.pending_posts.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        if let Some(pending) = self.pending_comments.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        if let Some(pending) = self.pending_content.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        self.pending_post_rows = None;
        self.pending_subreddits = None;

        for flag in self.pending_media.values() {
            flag.store(true, Ordering::SeqCst);
        }
        self.pending_media.clear();
        self.pending_media_order.clear();

        self.feed_cache.clear();
        self.comment_cache.clear();
        self.content_cache.clear();
        self.post_rows.clear();
        self.post_rows_width = 0;

        self.media_previews.clear();
        self.media_layouts.clear();
        self.media_failures.clear();
        self.pending_kitty_deletes.clear();
        self.active_kitty = None;
        self.needs_kitty_flush = false;
        self.content_area = None;

        self.posts.clear();
        self.post_offset.set(0);
        self.numeric_jump = None;
        self.content_scroll = 0;
        self.content = self.fallback_content.clone();
        self.content_source = self.fallback_source.clone();

        self.comments.clear();
        self.collapsed_comments.clear();
        self.visible_comment_indices.clear();
        self.comment_offset.set(0);
        self.selected_comment = 0;
        self.comment_status = "Select a post to load comments.".to_string();
        self.comment_sort_selected = false;

        self.selected_post = 0;

        self.reset_navigation_defaults();
        self.close_action_menu(None);

        self.needs_redraw = true;
    }

    fn reset_navigation_defaults(&mut self) {
        self.subreddits = vec![
            "r/frontpage".to_string(),
            "r/all".to_string(),
            "r/popular".to_string(),
        ];
        self.selected_sub = 0;
        self.subreddit_offset.set(0);
        let nav_len = NAV_SORTS.len().saturating_add(self.subreddits.len());
        if nav_len > 0 {
            let desired = NAV_SORTS.len().saturating_add(self.selected_sub);
            self.nav_index = desired.min(nav_len - 1);
            self.nav_mode = NavMode::Subreddits;
        } else {
            self.nav_index = 0;
            self.nav_mode = NavMode::Sorts;
        }
        self.ensure_subreddit_visible();
    }

    fn scoped_comment_cache_mut(
        &mut self,
        key: &CommentCacheKey,
    ) -> Option<&mut CommentCacheEntry> {
        if let Some(entry) = self.comment_cache.get(key) {
            if entry.scope != self.cache_scope {
                self.comment_cache.remove(key);
                return None;
            }
        }
        self.comment_cache.get_mut(key)
    }

    fn show_credentials_form(&mut self) -> Result<()> {
        self.menu_screen = MenuScreen::Credentials;
        self.menu_form = MenuForm::default();
        self.menu_form.focus(MenuField::ClientId);
        let mut error_message: Option<String> = None;
        match config::load(config::LoadOptions::default()) {
            Ok(cfg) => {
                let user_agent = if cfg.reddit.user_agent.trim().is_empty() {
                    config::RedditConfig::default().user_agent
                } else {
                    cfg.reddit.user_agent
                };
                self.menu_form.set_values(
                    cfg.reddit.client_id,
                    cfg.reddit.client_secret,
                    user_agent,
                );
            }
            Err(err) => {
                let default_agent = config::RedditConfig::default().user_agent;
                self.menu_form
                    .set_values(String::new(), String::new(), default_agent);
                let message = format!("Failed to load existing config: {err}");
                self.menu_form.set_status(message.clone());
                error_message = Some(message);
            }
        }
        self.status_message = match error_message {
            Some(msg) => format!("Edit Reddit credentials. {}", msg),
            None => "Edit Reddit credentials. Enter saves; Esc returns to accounts.".to_string(),
        };
        self.menu_account_index = self.menu_accounts.len();
        self.mark_dirty();
        Ok(())
    }

    fn switch_active_account(&mut self, account_id: i64) -> Result<()> {
        let manager = self.ensure_session_manager()?;
        let session = manager.switch(account_id)?;
        self.session_manager = Some(manager.clone());
        self.setup_authenticated_services()?;

        self.join_states.entry(session.account.id).or_default();
        self.queue_join_status_check();
        self.adopt_cache_scope(CacheScope::Account(session.account.id));

        self.refresh_menu_accounts().ok();

        let display = if !session.account.display_name.trim().is_empty() {
            session.account.display_name.trim().to_string()
        } else if !session.account.username.trim().is_empty() {
            session.account.username.trim().to_string()
        } else {
            session.account.reddit_id.trim().to_string()
        };

        self.status_message = format!("Switching to {}...", display);
        self.mark_dirty();

        if let Err(err) = self.reload_subreddits() {
            self.status_message = format!(
                "Switched to {}, but failed to refresh subreddits: {}",
                display, err
            );
            self.mark_dirty();
            return Err(err);
        }

        if let Err(err) = self.reload_posts() {
            self.status_message = format!(
                "Switched to {}, but failed to refresh posts: {}",
                display, err
            );
            self.mark_dirty();
            return Err(err);
        }

        self.status_message = format!("Switched to {}.", display);
        self.mark_dirty();
        Ok(())
    }
    fn mark_dirty(&mut self) {
        self.needs_redraw = true;
        if let Some(post) = self.posts.get(self.selected_post) {
            if let Some(preview) = self.media_previews.get(&post.post.name) {
                if preview.has_kitty() {
                    self.needs_kitty_flush = true;
                }
            }
        }
    }

    fn append_status_message(&mut self, message: impl Into<String>) {
        let text = message.into();
        if text.trim().is_empty() {
            return;
        }
        if self.status_message.trim().is_empty() {
            self.status_message = text;
        } else {
            self.status_message = format!("{} · {}", self.status_message, text);
        }
        self.mark_dirty();
    }

    fn prepend_status_message(&mut self, message: impl Into<String>) {
        let text = message.into();
        if text.trim().is_empty() {
            return;
        }
        if self.status_message.trim().is_empty() {
            self.status_message = text;
        } else {
            self.status_message = format!("{} · {}", text, self.status_message);
        }
        self.mark_dirty();
    }

    fn dismiss_release_note(&mut self) {
        if self.release_note_active {
            self.release_note_active = false;
        }
    }

    fn show_release_note_in_content(&mut self, note: &release_notes::ReleaseNote) {
        self.release_note_active = true;
        self.content = Self::release_note_text(note);
        self.content_source = format!("Release notes {}", note.version);
        self.content_scroll = 0;
        self.focused_pane = Pane::Content;
        self.mark_dirty();
    }

    fn release_note_text(note: &release_notes::ReleaseNote) -> Text<'static> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            note.title.clone(),
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("Version {}", note.version),
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .add_modifier(Modifier::ITALIC),
        )]));
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::styled(
            note.banner.clone(),
            Style::default()
                .fg(COLOR_TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::default());
        for detail in &note.details {
            lines.push(Line::from(vec![
                Span::styled("• ".to_string(), Style::default().fg(COLOR_ACCENT)),
                Span::styled(detail.clone(), Style::default().fg(COLOR_TEXT_PRIMARY)),
            ]));
        }
        if !note.details.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![Span::styled(
            "Press m → Release notes to revisit this message.".to_string(),
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .add_modifier(Modifier::ITALIC),
        )]));
        lines.push(Line::from(vec![
            Span::styled(
                "Full release notes: ".to_string(),
                Style::default().fg(COLOR_TEXT_SECONDARY),
            ),
            Span::styled(note.release_url.clone(), Style::default().fg(COLOR_ACCENT)),
        ]));
        Text::from(lines)
    }

    fn focus_status_for(pane: Pane) -> String {
        match pane {
            Pane::Comments => {
                "Focused Comments pane — press c to fold threads (Shift+C expands all).".to_string()
            }
            _ => format!("Focused {} pane", pane.title()),
        }
    }

    fn active_kitty_matches(&self, post_name: &str) -> bool {
        self.active_kitty
            .as_ref()
            .is_some_and(|active| active.post_name == post_name)
    }

    fn prepare_active_kitty_delete(&mut self) -> Option<String> {
        let active = self.active_kitty.take()?;
        if let Some(preview) = self.media_previews.get_mut(&active.post_name) {
            if let Some(kitty) = preview.kitty_mut() {
                if kitty.id == active.image_id {
                    if !kitty.transmitted {
                        return None;
                    }
                    kitty.transmitted = false;
                    return Some(kitty.delete_sequence());
                }
            }
        }
        Some(KittyImage::delete_sequence_for(
            active.image_id,
            active.wrap_tmux,
        ))
    }

    fn queue_active_kitty_delete(&mut self) {
        self.stop_active_video(None, true);
        if let Some(sequence) = self.prepare_active_kitty_delete() {
            self.pending_kitty_deletes.push(sequence);
            self.needs_kitty_flush = true;
            self.needs_redraw = true;
        }
    }

    fn emit_active_kitty_delete(
        &mut self,
        backend: &mut CrosstermBackend<Stdout>,
    ) -> io::Result<()> {
        if let Some(sequence) = self.prepare_active_kitty_delete() {
            crossterm::queue!(backend, Print(sequence))?;
            backend.flush()?;
        }
        Ok(())
    }

    fn flush_pending_kitty_deletes(
        &mut self,
        backend: &mut CrosstermBackend<Stdout>,
    ) -> io::Result<()> {
        if self.pending_kitty_deletes.is_empty() {
            return Ok(());
        }
        for sequence in self.pending_kitty_deletes.drain(..) {
            crossterm::queue!(backend, Print(sequence))?;
        }
        backend.flush()
    }

    fn selected_post_has_inline_media(&self) -> bool {
        let Some(post) = self.posts.get(self.selected_post) else {
            return false;
        };
        let Some(preview) = self.media_previews.get(&post.post.name) else {
            return false;
        };
        if preview.has_kitty() {
            return true;
        }
        if !self.kitty_status.is_enabled() {
            return false;
        }
        preview.has_video()
    }

    fn can_toggle_fullscreen_preview(&self) -> bool {
        if self.media_fullscreen {
            return true;
        }
        if self.banner_selected() {
            return false;
        }
        if !self.kitty_status.is_enabled() {
            return false;
        }
        let Some(post) = self.posts.get(self.selected_post) else {
            return false;
        };
        select_preview_source(&post.post).is_some()
    }

    pub fn new(opts: Options) -> Self {
        let current_version = resolve_current_version();
        let markdown = markdown::Renderer::new();
        let fallback_content = markdown.render(&opts.content);
        let (response_tx, response_rx) = unbounded();
        let mut model = Self {
            status_message: opts.status_message.clone(),
            subreddits: opts.subreddits.clone(),
            posts: opts.posts.clone(),
            feed_after: None,
            comments: Vec::new(),
            visible_comment_indices: Vec::new(),
            collapsed_comments: HashSet::new(),
            content: fallback_content.clone(),
            fallback_content,
            fallback_source: opts.content.clone(),
            content_source: opts.content.clone(),
            media_previews: HashMap::new(),
            media_failures: HashSet::new(),
            pending_media: HashMap::new(),
            pending_media_order: VecDeque::new(),
            pending_video: None,
            pending_video_clear: None,
            pending_external_video: None,
            video_completed_post: None,
            media_save_in_progress: None,
            media_layouts: HashMap::new(),
            media_handle: opts.media_handle.clone(),
            media_constraints: MediaConstraints {
                cols: MAX_IMAGE_COLS,
                rows: MAX_IMAGE_ROWS,
            },
            terminal_cols: 80,
            terminal_rows: 24,
            media_fullscreen: false,
            media_fullscreen_prev_focus: None,
            media_fullscreen_prev_scroll: None,
            feed_cache: HashMap::new(),
            comment_cache: HashMap::new(),
            post_rows: HashMap::new(),
            post_rows_width: 0,
            pending_post_rows: None,
            content_cache: HashMap::new(),
            pending_content: None,
            cache_scope: CacheScope::Anonymous,
            selected_sub: 0,
            selected_post: 0,
            selected_comment: 0,
            post_offset: Cell::new(0),
            post_view_height: Cell::new(0),
            comment_offset: Cell::new(0),
            comment_view_height: Cell::new(0),
            comment_view_width: Cell::new(0),
            comment_status_height: Cell::new(0),
            subreddit_offset: Cell::new(0),
            subreddit_view_height: Cell::new(0),
            subreddit_view_width: Cell::new(0),
            nav_index: 0,
            nav_mode: NavMode::Subreddits,
            content_scroll: 0,
            content_area: None,
            needs_kitty_flush: false,
            pending_kitty_deletes: Vec::new(),
            active_kitty: None,
            interaction_service: opts.interaction_service.clone(),
            feed_service: opts.feed_service.clone(),
            subreddit_service: opts.subreddit_service.clone(),
            comment_service: opts.comment_service.clone(),
            sort: opts.default_sort,
            comment_sort: opts.default_comment_sort,
            comment_sort_selected: false,
            focused_pane: Pane::Posts,
            menu_visible: false,
            menu_screen: MenuScreen::Accounts,
            menu_form: MenuForm::default(),
            menu_accounts: Vec::new(),
            menu_account_index: 0,
            action_menu_visible: false,
            help_visible: false,
            action_menu_mode: ActionMenuMode::Root,
            action_menu_items: Vec::new(),
            action_menu_selected: 0,
            action_link_items: Vec::new(),
            join_states: HashMap::new(),
            update_notice: None,
            update_check_in_progress: false,
            update_checked: false,
            update_install_in_progress: false,
            update_banner_selected: false,
            update_install_finished: false,
            release_note: release_notes::latest_for(&current_version),
            release_note_unread: false,
            release_note_active: false,
            latest_known_version: None,
            current_version: current_version.clone(),
            store: opts.store.clone(),
            session_manager: opts.session_manager.clone(),
            login_in_progress: false,
            needs_redraw: true,
            numeric_jump: None,
            spinner: Spinner::new(),
            config_path: opts.config_path.clone(),
            comment_status: "Select a post to load comments.".to_string(),
            comment_composer: None,
            response_tx,
            response_rx,
            next_request_id: 1,
            pending_posts: None,
            pending_comments: None,
            pending_comment_submit: None,
            pending_subreddits: None,
            needs_video_refresh: false,
            active_video: None,
            clipboard: None,
            kitty_status: KittyStatus::Unknown,
            kitty_probe_in_progress: false,
            show_nsfw: true,
        };
        model.cache_scope = model.current_cache_scope();
        model.subreddits = model
            .subreddits
            .drain(..)
            .map(|name| normalize_subreddit_name(&name))
            .collect();
        if model.subreddits.is_empty() {
            model.subreddits = vec!["r/frontpage".into(), "r/all".into(), "r/popular".into()];
        } else {
            ensure_core_subreddits(&mut model.subreddits);
        }

        model.selected_sub = model
            .subreddits
            .iter()
            .position(|name| name.eq_ignore_ascii_case("r/frontpage"))
            .unwrap_or(0);
        model.selected_post = 0;
        model.selected_comment = 0;
        model.post_offset.set(0);
        model.comment_offset.set(0);
        model.comment_view_height.set(0);
        model.content_scroll = 0;

        if !model.posts.is_empty() {
            model.sync_content_from_selection();
        }

        let nav_len = NAV_SORTS.len().saturating_add(model.subreddits.len());
        if nav_len > 0 {
            let desired = NAV_SORTS.len().saturating_add(model.selected_sub);
            model.nav_index = desired.min(nav_len - 1);
            model.nav_mode = NavMode::Subreddits;
        } else {
            model.nav_index = 0;
            model.nav_mode = NavMode::Sorts;
        }
        model.ensure_subreddit_visible();

        match model.store.show_nsfw_posts() {
            Ok(Some(preference)) => {
                model.show_nsfw = preference;
            }
            Ok(None) => {}
            Err(err) => {
                if model.status_message.is_empty() {
                    model.status_message = format!("NSFW preference load failed: {err}");
                } else {
                    model.status_message = format!(
                        "{} (NSFW preference load failed: {err})",
                        model.status_message
                    );
                }
            }
        }

        model.initialize_kitty_detection();

        if let Err(err) = model.reload_posts() {
            model.status_message = format!("Failed to load posts: {err}");
            model.content = model.fallback_content.clone();
            model.content_source = model.fallback_source.clone();
        }

        if opts.fetch_subreddits_on_start {
            if let Err(err) = model.reload_subreddits() {
                model.status_message = format!("Failed to refresh subreddits: {err}");
            }
        }

        if let Some(note) = model.release_note.clone() {
            let mut should_announce = true;
            match model.store.last_seen_release_version() {
                Ok(Some(raw)) => {
                    if let Ok(seen) = Version::parse(raw.trim()) {
                        if seen >= note.version {
                            should_announce = false;
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    model.append_status_message(format!("Release note status load failed: {err}"));
                }
            }
            if should_announce {
                let banner = format!("{} · Press m → Release notes for details.", note.banner);
                model.prepend_status_message(banner);
                model.release_note_unread = true;
                model.show_release_note_in_content(&note);
                if let Err(err) = model
                    .store
                    .set_last_seen_release_version(&note.version.to_string())
                {
                    model.append_status_message(format!(
                        "Failed to persist release note status: {err}"
                    ));
                }
            }
        }

        model.ensure_post_visible();
        model.queue_update_check();
        model.queue_join_status_check();
        model
    }

    pub fn run(&mut self) -> Result<()> {
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        stdout.execute(EnterAlternateScreen)?;
        stdout.execute(EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let result = self.event_loop(&mut terminal);
        let cleanup_result = self.cleanup_inline_media(terminal.backend_mut());

        terminal.backend_mut().execute(DisableMouseCapture)?;
        disable_raw_mode()?;
        terminal.backend_mut().execute(LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        result.and(cleanup_result)
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let mut last_tick = Instant::now();
        let tick_rate = Duration::from_millis(120);

        loop {
            self.poll_active_video();

            if self.poll_async() {
                self.mark_dirty();
            }

            if self.pending_video_clear.is_some() || !self.pending_kitty_deletes.is_empty() {
                self.flush_inline_clears(terminal.backend_mut())?;
            }

            if self.needs_redraw {
                terminal.draw(|frame| self.draw(frame))?;
                self.flush_inline_images(terminal.backend_mut())?;
                self.needs_redraw = false;
            }

            if let Err(err) = self.refresh_inline_video() {
                self.status_message = format!("Video preview error: {}", err);
                self.mark_dirty();
            }

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_millis(16));

            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match self.handle_key(key) {
                            Ok(true) => break,
                            Ok(false) => {}
                            Err(err) => {
                                self.status_message = format!("Error: {}", err);
                                self.mark_dirty();
                            }
                        }
                    }
                    Event::Mouse(mouse) => {
                        if let Err(err) = self.handle_mouse(mouse) {
                            self.status_message = format!("Error: {}", err);
                            self.mark_dirty();
                        }
                    }
                    _ => {}
                }
            }

            if self.poll_async() {
                self.mark_dirty();
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
                let mut ticked = false;
                if self.is_loading() && self.spinner.advance() {
                    ticked = true;
                } else if !self.is_loading() {
                    self.spinner.reset();
                }
                if self.login_in_progress {
                    ticked = true;
                }
                if ticked {
                    self.mark_dirty();
                }
            }
        }

        Ok(())
    }

    fn visible_panes(&self) -> [Pane; 3] {
        match self.focused_pane {
            Pane::Navigation => [Pane::Navigation, Pane::Posts, Pane::Content],
            Pane::Posts | Pane::Content | Pane::Comments => {
                [Pane::Posts, Pane::Content, Pane::Comments]
            }
        }
    }

    fn commit_navigation_selection(&mut self) -> Result<()> {
        match self.nav_mode {
            NavMode::Sorts => {
                let target = self
                    .subreddits
                    .get(self.selected_sub)
                    .cloned()
                    .unwrap_or_else(|| "r/frontpage".to_string());
                self.status_message =
                    format!("Refreshing {} sorted by {}…", target, sort_label(self.sort));
                self.reload_posts()?;
            }
            NavMode::Subreddits => {
                if self.subreddits.is_empty() {
                    return Ok(());
                }
                let index = self.nav_index.min(self.subreddits.len().saturating_sub(1));
                if self.selected_sub != index {
                    self.selected_sub = index;
                    if let Some(name) = self.subreddits.get(index) {
                        self.status_message =
                            format!("Loading {} ({})…", name, sort_label(self.sort));
                    }
                    self.reload_posts()?;
                } else if let Some(name) = self.subreddits.get(index) {
                    self.status_message =
                        format!("{} is already loaded. Press r to refresh if needed.", name);
                }
            }
        }
        self.mark_dirty();
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        let code = key.code;

        if self.active_video.is_some() && matches!(code, KeyCode::Esc) {
            let stopped = self.stop_active_video(Some("Video preview stopped."), false);
            if stopped {
                self.needs_video_refresh = false;
            }
            return Ok(false);
        }

        if self.active_video.is_some() && self.handle_video_controls(key)? {
            return Ok(false);
        }

        if self.media_fullscreen && matches!(code, KeyCode::Esc) {
            self.toggle_media_fullscreen()?;
            return Ok(false);
        }

        if self.comment_composer.is_some() {
            return self.handle_comment_composer_key(key);
        }

        if self.menu_visible {
            return self.handle_menu_key(code);
        }

        if self.action_menu_visible {
            return self.handle_action_menu_key(key);
        }

        if self.help_visible {
            return self.handle_help_key(key);
        }

        let mut dirty = false;

        if !matches!(code, KeyCode::Char(ch) if ch.is_ascii_digit()) {
            self.numeric_jump = None;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.open_menu()?;
                dirty = true;
            }
            KeyCode::Char('?') => {
                self.open_help();
                return Ok(false);
            }
            KeyCode::Char('g') | KeyCode::Char('G') => {
                self.open_navigation_mode(String::new(), true);
                return Ok(false);
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.reload_posts()?;
                dirty = true;
            }
            KeyCode::Char('s') => {
                self.reload_subreddits()?;
                dirty = true;
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                if self.focused_pane == Pane::Comments {
                    if self.posts.get(self.selected_post).is_some()
                        && self.comment_service.is_some()
                        && !self.banner_selected()
                    {
                        self.comment_sort_selected = true;
                        self.status_message = format!(
                            "Comment sort selected ({}). Use ←/→ or 1-{} to change; j/k returns to comments.",
                            comment_sort_label(self.comment_sort),
                            COMMENT_SORTS.len()
                        );
                        self.mark_dirty();
                        return Ok(false);
                    } else {
                        self.status_message =
                            "Load a post with comments before adjusting the sort.".to_string();
                        dirty = true;
                    }
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                self.open_action_menu();
                return Ok(false);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.toggle_nsfw_filter()?;
                dirty = true;
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.toggle_media_fullscreen()?;
                }
            }
            KeyCode::Char('u') => {
                if self.banner_selected() {
                    self.status_message = "Select a post before voting.".to_string();
                } else if self.focused_pane == Pane::Comments {
                    let new_dir = self
                        .selected_comment_index()
                        .and_then(|idx| self.comments.get(idx))
                        .map(|entry| toggle_vote_value(vote_from_likes(entry.likes), 1))
                        .unwrap_or(1);
                    self.vote_selected_comment(new_dir);
                } else {
                    let old = self
                        .posts
                        .get(self.selected_post)
                        .map(|post| vote_from_likes(post.post.likes))
                        .unwrap_or(0);
                    let new_dir = toggle_vote_value(old, 1);
                    self.vote_selected_post(new_dir);
                }
                dirty = true;
            }
            KeyCode::Char('U') => {
                self.install_update()?;
                dirty = true;
            }
            KeyCode::Char('d') => {
                if self.banner_selected() {
                    self.status_message = "Select a post before voting.".to_string();
                } else if self.focused_pane == Pane::Comments {
                    let new_dir = self
                        .selected_comment_index()
                        .and_then(|idx| self.comments.get(idx))
                        .map(|entry| toggle_vote_value(vote_from_likes(entry.likes), -1))
                        .unwrap_or(-1);
                    self.vote_selected_comment(new_dir);
                } else {
                    let old = self
                        .posts
                        .get(self.selected_post)
                        .map(|post| vote_from_likes(post.post.likes))
                        .unwrap_or(0);
                    let new_dir = toggle_vote_value(old, -1);
                    self.vote_selected_post(new_dir);
                }
                dirty = true;
            }
            KeyCode::Char('S') => {
                self.save_high_res_media()?;
            }
            KeyCode::Char('w') => {
                self.open_comment_composer()?;
                return Ok(false);
            }
            KeyCode::Char('c') => {
                if self.focused_pane == Pane::Comments {
                    self.toggle_selected_comment_fold();
                    dirty = true;
                }
            }
            KeyCode::Char('C') => {
                if self.focused_pane == Pane::Comments {
                    self.expand_all_comments();
                    dirty = true;
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if self.focused_pane == Pane::Comments && !self.comment_sort_selected {
                    if let Err(err) = self.copy_selected_comment() {
                        self.status_message = format!("Failed to copy comment: {err}");
                        self.mark_dirty();
                    }
                }
            }
            KeyCode::Enter => {
                if self.focused_pane == Pane::Navigation {
                    self.commit_navigation_selection()?;
                    dirty = true;
                } else if self.focused_pane == Pane::Posts && self.banner_selected() {
                    self.install_update()?;
                    dirty = true;
                } else if self.focused_pane == Pane::Comments && !self.comment_sort_selected {
                    let open_root = self
                        .selected_comment_index()
                        .and_then(|idx| self.comments.get(idx))
                        .map(|entry| entry.is_post_root)
                        .unwrap_or(true);
                    if open_root {
                        self.open_comment_composer()?;
                        return Ok(false);
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.focused_pane == Pane::Navigation && matches!(self.nav_mode, NavMode::Sorts)
                {
                    self.shift_sort(-1)?;
                    dirty = true;
                } else if self.focused_pane == Pane::Comments && self.comment_sort_selected {
                    self.shift_comment_sort(-1)?;
                    dirty = true;
                } else {
                    let previous = self.focused_pane.previous();
                    if previous != self.focused_pane {
                        self.focused_pane = previous;
                        if self.focused_pane == Pane::Navigation {
                            self.close_action_menu(None);
                        }
                        self.status_message = Self::focus_status_for(self.focused_pane);
                        dirty = true;
                    }
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if self.focused_pane == Pane::Navigation && matches!(self.nav_mode, NavMode::Sorts)
                {
                    self.shift_sort(1)?;
                    dirty = true;
                } else if self.focused_pane == Pane::Comments && self.comment_sort_selected {
                    self.shift_comment_sort(1)?;
                    dirty = true;
                } else {
                    let next = self.focused_pane.next();
                    if next != self.focused_pane {
                        self.focused_pane = next;
                        if self.focused_pane == Pane::Navigation {
                            self.close_action_menu(None);
                        }
                        self.status_message = Self::focus_status_for(self.focused_pane);
                        dirty = true;
                    }
                }
            }
            KeyCode::Char(ch @ '1'..='6')
                if (self.focused_pane == Pane::Navigation
                    && matches!(self.nav_mode, NavMode::Sorts))
                    || (self.focused_pane == Pane::Comments && self.comment_sort_selected) =>
            {
                let idx = (ch as u8 - b'1') as usize;
                if self.focused_pane == Pane::Navigation {
                    self.set_sort_by_index(idx)?;
                } else {
                    self.set_comment_sort_by_index(idx)?;
                }
                dirty = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.navigate_in_focus(1)?;
                dirty = true;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.navigate_in_focus(-1)?;
                dirty = true;
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                let step = if self.focused_pane == Pane::Posts {
                    self.posts_page_step()
                } else {
                    5
                };
                if step != 0 {
                    self.navigate_in_focus(step)?;
                    dirty = true;
                }
            }
            KeyCode::PageUp => {
                let step = if self.focused_pane == Pane::Posts {
                    self.posts_page_step()
                } else {
                    5
                };
                if step != 0 {
                    self.navigate_in_focus(-step)?;
                    dirty = true;
                }
            }
            KeyCode::Home => {
                if self.focused_pane == Pane::Posts {
                    if self.posts.is_empty() {
                        self.status_message = "No posts available to select.".to_string();
                    } else {
                        self.select_post_at(0);
                        dirty = true;
                        self.status_message = "Jumped to first post.".to_string();
                    }
                }
            }
            KeyCode::End => {
                if self.focused_pane == Pane::Posts {
                    if self.posts.is_empty() {
                        self.status_message = "No posts available to select.".to_string();
                    } else {
                        let last = self.posts.len() - 1;
                        self.select_post_at(last);
                        dirty = true;
                        self.status_message = format!("Jumped to post #{}.", last + 1);
                    }
                }
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                if self.focused_pane == Pane::Posts {
                    let now = Instant::now();
                    let digit = ch.to_digit(10).unwrap() as usize;
                    let timeout = Duration::from_millis(800);
                    let (base, continuing) = match &self.numeric_jump {
                        Some(jump) if now.duration_since(jump.last_input) <= timeout => {
                            (jump.value, true)
                        }
                        _ => (0, false),
                    };
                    let new_value = if continuing {
                        base.saturating_mul(10).saturating_add(digit)
                    } else if digit == 0 {
                        10
                    } else {
                        digit
                    };
                    self.numeric_jump = Some(NumericJump {
                        value: new_value,
                        last_input: now,
                    });

                    if self.posts.is_empty() {
                        self.status_message = "No posts available to select.".to_string();
                    } else {
                        let max_index = self.posts.len() - 1;
                        let target = new_value.saturating_sub(1);
                        if target > max_index {
                            self.status_message = format!(
                                "Only {} post{} loaded right now.",
                                self.posts.len(),
                                if self.posts.len() == 1 {
                                    " is"
                                } else {
                                    "s are"
                                }
                            );
                        } else {
                            let previous = self.selected_post;
                            self.select_post_at(target);
                            self.status_message = if self.selected_post != previous {
                                format!("Selected post #{}.", self.selected_post + 1)
                            } else {
                                format!("Already on post #{}.", self.selected_post + 1)
                            };
                        }
                    }
                    dirty = true;
                }
            }
            _ => {}
        }

        if dirty {
            self.mark_dirty();
        }
        Ok(false)
    }

    fn handle_video_controls(&mut self, key: KeyEvent) -> Result<bool> {
        if self.active_video.is_none() {
            return Ok(false);
        }

        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Ok(false);
        }

        let command = match key.code {
            KeyCode::Char(' ') | KeyCode::Char('p') | KeyCode::Char('P') => {
                Some((VideoCommand::TogglePause, "Toggled inline video playback."))
            }
            KeyCode::Char('[') | KeyCode::Char('{') => Some((
                VideoCommand::SeekRelative(-5.0),
                "Rewound inline video 5 seconds.",
            )),
            KeyCode::Char(']') | KeyCode::Char('}') => Some((
                VideoCommand::SeekRelative(5.0),
                "Advanced inline video 5 seconds.",
            )),
            _ => None,
        };

        let Some((command, status)) = command else {
            return Ok(false);
        };

        let controls_supported = self
            .active_video
            .as_ref()
            .is_some_and(|active| active.session.controls_supported());
        if !controls_supported {
            self.status_message =
                "Inline video controls aren’t supported on this platform.".to_string();
            self.mark_dirty();
            return Ok(true);
        }

        let status_text = status.to_string();
        let result = {
            let active = self
                .active_video
                .as_mut()
                .expect("active video missing after controls_supported check");
            active.session.send_command(command)
        };

        match result {
            Ok(()) => {
                self.status_message = status_text;
                self.mark_dirty();
            }
            Err(err) => {
                let control_err = err.to_string();
                match self.restart_inline_video() {
                    Ok(()) => {
                        self.status_message = format!(
                            "Inline video control failed: {}; restarted preview.",
                            control_err
                        );
                    }
                    Err(restart_err) => {
                        self.status_message = format!(
                            "Inline video control failed: {}; restart failed: {}",
                            control_err, restart_err
                        );
                    }
                }
                self.mark_dirty();
            }
        }

        Ok(true)
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> Result<()> {
        if self.menu_visible
            || self.action_menu_visible
            || self.help_visible
            || self.comment_composer.is_some()
        {
            return Ok(());
        }

        self.numeric_jump = None;

        match event.kind {
            MouseEventKind::ScrollDown => {
                self.navigate_in_focus(1)?;
                self.mark_dirty();
            }
            MouseEventKind::ScrollUp => {
                self.navigate_in_focus(-1)?;
                self.mark_dirty();
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_menu_key(&mut self, code: KeyCode) -> Result<bool> {
        match self.menu_screen {
            MenuScreen::Accounts => self.handle_menu_accounts_key(code),
            MenuScreen::Credentials => self.handle_menu_credentials_key(code),
            MenuScreen::ReleaseNotes => self.handle_menu_release_notes_key(code),
        }
    }

    fn handle_menu_accounts_key(&mut self, code: KeyCode) -> Result<bool> {
        let positions = self.menu_account_positions();
        let option_count = positions.total;
        let add_index = positions.add;
        let join_index = positions.join;
        let release_index = positions.release_notes;
        let update_index = positions.update_check;
        let install_index = positions.install;
        let github_index = positions.github;
        let support_index = positions.support;

        if option_count == 0 {
            return Ok(false);
        }

        match code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Esc => {
                self.menu_visible = false;
                self.status_message = "Guided menu closed.".to_string();
                self.mark_dirty();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.menu_account_index + 1 < option_count {
                    self.menu_account_index += 1;
                    self.mark_dirty();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.menu_account_index > 0 {
                    self.menu_account_index -= 1;
                    self.mark_dirty();
                }
            }
            KeyCode::Home => {
                self.menu_account_index = 0;
                self.mark_dirty();
            }
            KeyCode::End => {
                self.menu_account_index = option_count - 1;
                self.mark_dirty();
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.menu_account_index = add_index;
                self.show_credentials_form()?;
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if let Some(idx) = release_index {
                    self.menu_account_index = idx;
                    self.show_release_notes_screen()?;
                }
            }
            KeyCode::Enter => {
                if self.menu_account_index < self.menu_accounts.len() {
                    let account_id = self.menu_accounts[self.menu_account_index].id;
                    match self.switch_active_account(account_id) {
                        Ok(()) => {
                            self.menu_visible = false;
                            self.mark_dirty();
                        }
                        Err(err) => {
                            self.status_message = format!("Failed to switch account: {err}");
                            self.mark_dirty();
                        }
                    }
                } else if self.menu_account_index == add_index {
                    self.show_credentials_form()?;
                } else if self.menu_account_index == join_index {
                    self.join_reddix_subreddit()?;
                } else if release_index.is_some_and(|idx| self.menu_account_index == idx) {
                    self.show_release_notes_screen()?;
                } else if self.menu_account_index == update_index {
                    self.force_update_check();
                } else if install_index.is_some_and(|idx| self.menu_account_index == idx) {
                    self.install_update()?;
                } else if self.menu_account_index == github_index {
                    let _ = self.open_project_link();
                } else if self.menu_account_index == support_index {
                    let _ = self.open_support_link();
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_menu_credentials_key(&mut self, code: KeyCode) -> Result<bool> {
        let mut dirty = false;
        match code {
            KeyCode::Esc => {
                self.menu_screen = MenuScreen::Accounts;
                if let Err(err) = self.refresh_menu_accounts() {
                    self.status_message = format!("Guided menu: failed to list accounts: {}", err);
                } else {
                    if let Some(pos) = self.menu_accounts.iter().position(|entry| entry.is_active) {
                        self.menu_account_index = pos;
                    } else {
                        self.menu_account_index = 0;
                    }
                    self.status_message =
                        "Guided menu: j/k select account · Enter switch · a add · Esc/m close"
                            .to_string();
                }
                self.mark_dirty();
                return Ok(false);
            }
            KeyCode::Tab | KeyCode::Down => {
                self.menu_form.next();
                dirty = true;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.menu_form.previous();
                dirty = true;
            }
            KeyCode::Enter => match self.menu_form.active {
                MenuField::Save => {
                    let (client_id, client_secret, user_agent) = self.menu_form.trimmed_values();
                    match config::save_reddit_credentials(
                        None,
                        &client_id,
                        &client_secret,
                        &user_agent,
                    ) {
                        Ok(path) => {
                            self.menu_form.set_values(
                                client_id.clone(),
                                client_secret.clone(),
                                user_agent.clone(),
                            );
                            self.menu_form.focus(MenuField::ClientId);
                            if self.login_in_progress {
                                let message = "Authorization already in progress. Complete it in your browser.".to_string();
                                self.menu_form.set_status(message.clone());
                                self.status_message = message;
                            } else if let Err(err) = self.start_authorization_flow(path.as_path()) {
                                let message =
                                    format!("Failed to start Reddit authorization: {err}");
                                self.menu_form.set_status(message.clone());
                                self.status_message = message;
                            }
                            dirty = true;
                        }
                        Err(err) => {
                            let message = format!("Failed to save credentials: {err}");
                            self.menu_form.set_status(message.clone());
                            self.status_message = message;
                            dirty = true;
                        }
                    }
                }
                MenuField::OpenLink => {
                    if let Err(err) = self.open_auth_link_in_browser() {
                        let message = format!("Failed to open authorization link: {err}");
                        self.menu_form.set_status(message.clone());
                        self.status_message = message;
                    }
                    dirty = true;
                }
                _ => {
                    self.menu_form.next();
                    dirty = true;
                }
            },
            KeyCode::Backspace => {
                self.menu_form.backspace();
                dirty = true;
            }
            KeyCode::Delete => {
                self.menu_form.clear_active();
                dirty = true;
            }
            KeyCode::Char(ch) => {
                if self.menu_form.active_accepts_text() {
                    if !ch.is_control() {
                        self.menu_form.insert_char(ch);
                        dirty = true;
                    }
                } else {
                    match ch {
                        'q' | 'Q' => return Ok(true),
                        'm' | 'M' => {
                            self.menu_visible = false;
                            self.status_message = "Guided menu closed.".to_string();
                            self.mark_dirty();
                            return Ok(false);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if dirty {
            self.mark_dirty();
        }
        Ok(false)
    }

    fn handle_menu_release_notes_key(&mut self, code: KeyCode) -> Result<bool> {
        match code {
            KeyCode::Esc => {
                self.menu_screen = MenuScreen::Accounts;
                self.mark_dirty();
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.menu_visible = false;
                self.status_message = "Guided menu closed.".to_string();
                self.mark_dirty();
            }
            KeyCode::Enter | KeyCode::Char('o') | KeyCode::Char('O') => {
                self.open_release_notes_link()?;
            }
            _ => {}
        }
        Ok(false)
    }

    fn collect_links_for_current_context(&self) -> Vec<LinkEntry> {
        let mut seen = HashSet::new();
        let mut collected = Vec::new();

        if let Some(post) = self.posts.get(self.selected_post) {
            for entry in &post.links {
                if seen.insert(entry.url.clone()) {
                    collected.push(entry.clone());
                }
            }
        }

        if !self.visible_comment_indices.is_empty() {
            let selection = self
                .selected_comment
                .min(self.visible_comment_indices.len().saturating_sub(1));
            if let Some(comment_index) = self.visible_comment_indices.get(selection) {
                if let Some(comment) = self.comments.get(*comment_index) {
                    for entry in &comment.links {
                        if seen.insert(entry.url.clone()) {
                            collected.push(entry.clone());
                        }
                    }
                }
            }
        }

        collected
    }

    fn save_high_res_media(&mut self) -> Result<()> {
        if self.banner_selected() {
            self.status_message = "Select a post before saving media.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        if self.media_save_in_progress.is_some() {
            self.status_message = "Media download already in progress.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let Some(post) = self.posts.get(self.selected_post) else {
            self.status_message =
                "No post selected. Select a post with media and try again.".to_string();
            self.mark_dirty();
            return Ok(());
        };

        let candidates = collect_high_res_media(&post.post);
        if candidates.is_empty() {
            self.status_message = "No downloadable media found for the selected post.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let dest_dir = default_download_dir();
        let total = candidates.len();

        self.media_save_in_progress = Some(MediaSaveJob { total });
        self.status_message = if total == 1 {
            "Saving media…".to_string()
        } else {
            format!("Saving {} files…", total)
        };
        self.spinner.reset();
        self.mark_dirty();

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let result = save_media_batch(candidates, dest_dir);
            let _ = tx.send(AsyncResponse::MediaSave { result });
        });

        Ok(())
    }

    fn build_action_menu_entries(&self) -> Vec<ActionMenuEntry> {
        let mut entries = Vec::new();

        let mut video_exists = false;
        let mut video_ready = false;
        let mut video_pending = false;
        if !self.banner_selected() {
            if let Some(post) = self.posts.get(self.selected_post) {
                if let Some(preview) = self.media_previews.get(&post.post.name) {
                    if preview.video().is_some() {
                        video_exists = true;
                        video_ready = true;
                    }
                } else if video::find_video_source(&post.post).is_some() {
                    video_exists = true;
                }
                if self
                    .pending_video
                    .as_ref()
                    .is_some_and(|pending| pending.post_name == post.post.name)
                {
                    video_exists = true;
                    video_ready = false;
                    video_pending = true;
                }
            }
        }

        let (video_label, video_action, video_enabled) = if !video_exists {
            (
                "Inline video (not available)".to_string(),
                ActionMenuAction::StartVideo,
                false,
            )
        } else if !self.kitty_status.is_enabled() {
            let pending = self.pending_external_video.is_some();
            let label = if pending {
                "Open video in mpv (launching…)"
            } else {
                "Open video in mpv"
            };
            (
                label.to_string(),
                ActionMenuAction::OpenVideoExternal,
                !pending,
            )
        } else if video_pending {
            (
                "Inline video (preparing…)".to_string(),
                ActionMenuAction::StartVideo,
                false,
            )
        } else if !video_ready {
            (
                "Inline video (loading…)".to_string(),
                ActionMenuAction::StartVideo,
                false,
            )
        } else if self.active_video.is_some() {
            (
                "Stop inline video".to_string(),
                ActionMenuAction::StopVideo,
                true,
            )
        } else {
            (
                "Play inline video".to_string(),
                ActionMenuAction::StartVideo,
                true,
            )
        };

        let mut video_entry = ActionMenuEntry::new(video_label, video_action);
        if !video_enabled {
            video_entry = video_entry.disabled();
        }
        entries.push(video_entry);

        let links = self.collect_links_for_current_context();
        let links_label = if links.is_empty() {
            "Open links… (none available)".to_string()
        } else {
            format!("Open links… ({} available)", links.len())
        };
        let mut links_entry = ActionMenuEntry::new(links_label, ActionMenuAction::OpenLinks);
        if links.is_empty() {
            links_entry = links_entry.disabled();
        }
        entries.push(links_entry);

        let media_available = if self.banner_selected() {
            false
        } else {
            self.posts
                .get(self.selected_post)
                .map(|preview| !collect_high_res_media(&preview.post).is_empty())
                .unwrap_or(false)
        };
        let mut media_label = "Save full-resolution media".to_string();
        if let Some(job) = &self.media_save_in_progress {
            media_label = format!("Saving media… ({} files)", job.total);
        }
        let mut media_entry = ActionMenuEntry::new(media_label, ActionMenuAction::SaveMedia);
        if !media_available || self.media_save_in_progress.is_some() {
            media_entry = media_entry.disabled();
        }
        entries.push(media_entry);

        let fullscreen_available = self.can_toggle_fullscreen_preview();
        let fullscreen_label = if self.media_fullscreen {
            "Exit fullscreen preview"
        } else {
            "View media fullscreen"
        };
        let mut fullscreen_entry =
            ActionMenuEntry::new(fullscreen_label, ActionMenuAction::ToggleFullscreen);
        if !fullscreen_available {
            fullscreen_entry = fullscreen_entry.disabled();
        }
        entries.push(fullscreen_entry);

        if self.comment_composer.is_some() {
            entries.push(
                ActionMenuEntry::new(
                    "Write a comment… (already composing)",
                    ActionMenuAction::ComposeComment,
                )
                .disabled(),
            );
        } else if self.pending_comment_submit.is_some() {
            entries.push(
                ActionMenuEntry::new(
                    "Write a comment… (posting…)",
                    ActionMenuAction::ComposeComment,
                )
                .disabled(),
            );
        } else if self.interaction_service.is_none() {
            entries.push(
                ActionMenuEntry::new(
                    "Write a comment… (sign in required)",
                    ActionMenuAction::ComposeComment,
                )
                .disabled(),
            );
        } else {
            match self.comment_target_for_context() {
                Ok(target) => {
                    let label = Self::comment_action_label(&target);
                    entries.push(ActionMenuEntry::new(
                        label,
                        ActionMenuAction::ComposeComment,
                    ));
                }
                Err(err) => {
                    entries.push(
                        ActionMenuEntry::new(
                            format!("Write a comment… ({})", err),
                            ActionMenuAction::ComposeComment,
                        )
                        .disabled(),
                    );
                }
            }
        }

        entries.push(ActionMenuEntry::new(
            "Search subreddits & users…",
            ActionMenuAction::OpenNavigation,
        ));

        entries
    }

    fn open_action_menu(&mut self) {
        self.queue_active_kitty_delete();
        self.action_menu_items = self.build_action_menu_entries();
        self.action_menu_selected = self
            .action_menu_items
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.enabled)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.action_link_items.clear();
        self.action_menu_mode = ActionMenuMode::Root;
        self.action_menu_visible = true;
        self.status_message =
            "Actions: j/k move · Ctrl+H/J/K/L navigate · Enter/l open · h/Esc/o back".to_string();
        self.mark_dirty();
    }

    fn close_action_menu(&mut self, message: Option<&str>) {
        if self.action_menu_visible {
            self.action_menu_visible = false;
            self.action_menu_mode = ActionMenuMode::Root;
            self.action_menu_items.clear();
            self.action_menu_selected = 0;
            self.action_link_items.clear();
            if let Some(msg) = message {
                self.status_message = msg.to_string();
            }
            self.mark_dirty();
        }
    }

    fn open_help(&mut self) {
        if self.help_visible {
            return;
        }
        self.help_visible = true;
        self.status_message = "Help: press Esc or ? to close".to_string();
        self.mark_dirty();
    }

    fn close_help(&mut self) {
        if self.help_visible {
            self.help_visible = false;
            self.status_message = "Help closed.".to_string();
            self.mark_dirty();
        }
    }

    fn open_navigation_mode(&mut self, filter: String, editing: bool) {
        self.queue_active_kitty_delete();
        let state = self.build_navigation_state(filter, editing);
        self.action_menu_items.clear();
        self.action_menu_selected = 0;
        self.action_link_items.clear();
        self.action_menu_mode = ActionMenuMode::Navigation(state);
        self.action_menu_visible = true;
        self.status_message =
            "Navigation: type to search · ↑/↓ choose · Enter/l open · Tab toggle typing · Esc clear/close · n toggle NSFW · h back · Ctrl+H/J/K/L navigate (even when typing)"
                .to_string();
        self.mark_dirty();
    }

    fn build_navigation_state(&self, filter: String, editing: bool) -> NavigationMenuState {
        let matches = self.navigation_matches(&filter);
        NavigationMenuState::new(filter, matches, editing)
    }

    fn navigation_matches(&self, filter: &str) -> Vec<NavigationMatch> {
        let trimmed = filter.trim();
        let mut matches = Vec::new();
        let mut seen = HashSet::new();
        let mut stored: Vec<(String, NavigationTarget, Option<String>)> = Vec::new();

        for name in &self.subreddits {
            let kind = classify_feed_target(name);
            let label = navigation_display_name(name);
            let (target, description) = match kind {
                FeedKind::FrontPage => (
                    NavigationTarget::Subreddit(name.clone()),
                    Some("front page".to_string()),
                ),
                FeedKind::Subreddit(_) => (
                    NavigationTarget::Subreddit(name.clone()),
                    Some("subscribed".to_string()),
                ),
                FeedKind::User(user) => (
                    NavigationTarget::User(user.to_string()),
                    Some("user profile".to_string()),
                ),
                FeedKind::Search(query) => (
                    NavigationTarget::Search(query.to_string()),
                    Some("recent search".to_string()),
                ),
            };
            stored.push((label, target, description));
        }

        if trimmed.is_empty() {
            for (label, target, description) in &stored {
                let mut entry = NavigationMatch::new(label.clone(), target.clone());
                if let Some(desc) = description {
                    entry = entry.with_description(desc.clone());
                }
                push_navigation_entry(&mut matches, &mut seen, entry);
            }
            return matches;
        }

        let trimmed_lower = trimmed.to_ascii_lowercase();

        let normalized = normalize_subreddit_name(trimmed);
        let direct = NavigationMatch::new(
            format!("Open {}", normalized),
            NavigationTarget::Subreddit(normalized.clone()),
        )
        .with_description("typed subreddit");
        push_navigation_entry(&mut matches, &mut seen, direct);

        if let Some(user_target) = normalize_user_target(trimmed) {
            let entry = NavigationMatch::new(
                format!("Open {}", user_target),
                NavigationTarget::User(user_target.trim_start_matches("u/").to_string()),
            )
            .with_description("user profile");
            push_navigation_entry(&mut matches, &mut seen, entry);
        }

        if let Some(search_target) = canonical_search_target(trimmed) {
            let term = search_target
                .trim_start_matches("search:")
                .trim()
                .to_string();
            let entry = NavigationMatch::new(
                format!("Search Reddit for \"{}\"", term),
                NavigationTarget::Search(term.clone()),
            )
            .with_description("search");
            push_navigation_entry(&mut matches, &mut seen, entry);
        }

        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<(i64, usize)> = Vec::new();
        for (index, (label, _, _)) in stored.iter().enumerate() {
            let label_lower = label.to_ascii_lowercase();
            if let Some(score) = matcher.fuzzy_match(&label_lower, &trimmed_lower) {
                scored.push((score, index));
            }
        }

        scored.sort_by(|a, b| b.0.cmp(&a.0));

        for (_, idx) in scored {
            let (label, target, description) = &stored[idx];
            let mut entry = NavigationMatch::new(label.clone(), target.clone());
            if let Some(desc) = description {
                entry = entry.with_description(desc.clone());
            }
            push_navigation_entry(&mut matches, &mut seen, entry);
        }

        matches
    }
    fn refresh_navigation_matches(&self, state: &mut NavigationMenuState) {
        state.matches = self.navigation_matches(&state.filter);
        state.ensure_selection();
    }

    fn activate_navigation_target(&mut self, target: &NavigationTarget) -> Result<()> {
        match target {
            NavigationTarget::Subreddit(name) => {
                let normalized = normalize_subreddit_name(name);
                if !self
                    .subreddits
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&normalized))
                {
                    self.subreddits.push(normalized.clone());
                    self.subreddits
                        .sort_by_key(|name| name.to_ascii_lowercase());
                }
                self.select_subreddit_by_name(&normalized);
                let label = navigation_display_name(&normalized);
                self.status_message = format!("Loading {} ({})…", label, sort_label(self.sort));
                self.reload_posts()?;
                self.focused_pane = Pane::Posts;
                self.close_action_menu(None);
                self.mark_dirty();
            }
            NavigationTarget::User(username) => {
                let trimmed = username.trim();
                if trimmed.is_empty() {
                    self.status_message = "Provide a username to open.".to_string();
                    self.mark_dirty();
                    return Ok(());
                }
                let canonical = format!("u/{}", trimmed);
                if !self
                    .subreddits
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&canonical))
                {
                    self.subreddits.push(canonical.clone());
                    self.subreddits
                        .sort_by_key(|name| name.to_ascii_lowercase());
                }
                self.select_subreddit_by_name(&canonical);
                let label = navigation_display_name(&canonical);
                self.status_message = format!("Loading {} ({})…", label, sort_label(self.sort));
                self.reload_posts()?;
                self.focused_pane = Pane::Posts;
                self.close_action_menu(None);
                self.mark_dirty();
            }
            NavigationTarget::Search(query) => {
                let trimmed = query.trim();
                if trimmed.is_empty() {
                    self.status_message = "Enter a search term to continue.".to_string();
                    self.mark_dirty();
                    return Ok(());
                }
                let canonical = format!("search: {}", trimmed);
                if !self
                    .subreddits
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&canonical))
                {
                    self.subreddits.push(canonical.clone());
                    self.subreddits
                        .sort_by_key(|name| name.to_ascii_lowercase());
                }
                self.select_subreddit_by_name(&canonical);
                self.status_message = format!(
                    "Searching Reddit for \"{}\" ({})…",
                    trimmed,
                    sort_label(self.sort)
                );
                self.reload_posts()?;
                self.focused_pane = Pane::Posts;
                self.close_action_menu(None);
                self.mark_dirty();
            }
        }
        Ok(())
    }

    fn handle_action_links_key(&mut self, key: KeyEvent) -> Result<bool> {
        let code = key.code;

        if self.action_link_items.is_empty() {
            if matches!(
                code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Char('H')
            ) {
                self.action_menu_mode = ActionMenuMode::Root;
                self.action_menu_selected = 0;
                self.status_message =
                    "Actions: j/k move · Ctrl+H/J/K/L navigate · Enter/l open · h/Esc/o back"
                        .to_string();
                self.mark_dirty();
            }
            return Ok(false);
        }

        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Char('H') => {
                self.action_menu_mode = ActionMenuMode::Root;
                self.action_menu_selected = 0;
                self.status_message =
                    "Actions: j/k move · Ctrl+H/J/K/L navigate · Enter/l open · h/Esc/o back"
                        .to_string();
                self.mark_dirty();
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.action_menu_selected > 0 {
                    self.action_menu_selected -= 1;
                    self.mark_dirty();
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.action_menu_selected + 1 < self.action_link_items.len() {
                    self.action_menu_selected += 1;
                    self.mark_dirty();
                }
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                if self.action_menu_selected > 0 {
                    self.action_menu_selected -= 1;
                    self.mark_dirty();
                }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                if self.action_menu_selected + 1 < self.action_link_items.len() {
                    self.action_menu_selected += 1;
                    self.mark_dirty();
                }
            }
            KeyCode::PageUp => {
                if self.action_menu_selected > 0 {
                    let step = self.action_menu_selected.min(5);
                    self.action_menu_selected -= step;
                    self.mark_dirty();
                }
            }
            KeyCode::PageDown => {
                if self.action_menu_selected + 1 < self.action_link_items.len() {
                    let remaining = self.action_link_items.len() - self.action_menu_selected - 1;
                    let step = remaining.min(5);
                    self.action_menu_selected += step;
                    self.mark_dirty();
                }
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('L') => {
                let index = self
                    .action_menu_selected
                    .min(self.action_link_items.len().saturating_sub(1));
                let entry = &self.action_link_items[index];
                let label = entry.label.clone();
                let url = entry.url.clone();
                match webbrowser::open(&url) {
                    Ok(_) => {
                        let message = format!("Opened {label} in your browser.");
                        self.close_action_menu(Some(&message));
                        self.status_message = message;
                        self.mark_dirty();
                    }
                    Err(err) => {
                        self.status_message = format!("Failed to open {label}: {err} (URL: {url})");
                        self.mark_dirty();
                    }
                }
            }
            _ => {}
        }

        Ok(false)
    }

    fn handle_action_menu_key(&mut self, key: KeyEvent) -> Result<bool> {
        let code = key.code;
        let modifiers = key.modifiers;

        match self.action_menu_mode.clone() {
            ActionMenuMode::Root => {
                if self.action_menu_items.is_empty() {
                    if matches!(
                        code,
                        KeyCode::Esc
                            | KeyCode::Char('o')
                            | KeyCode::Char('O')
                            | KeyCode::Char('q')
                            | KeyCode::Char('h')
                            | KeyCode::Char('H')
                    ) {
                        self.close_action_menu(Some("Actions closed."));
                    }
                    return Ok(false);
                }

                match code {
                    KeyCode::Esc
                    | KeyCode::Char('o')
                    | KeyCode::Char('O')
                    | KeyCode::Char('q')
                    | KeyCode::Char('h')
                    | KeyCode::Char('H') => {
                        self.close_action_menu(Some("Actions closed."));
                    }
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                        if self.action_menu_selected > 0 {
                            self.action_menu_selected -= 1;
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                        if self.action_menu_selected + 1 < self.action_menu_items.len() {
                            self.action_menu_selected += 1;
                            self.mark_dirty();
                        }
                    }
                    KeyCode::PageUp => {
                        if self.action_menu_selected > 0 {
                            let step = self.action_menu_selected.min(5);
                            self.action_menu_selected -= step;
                            self.mark_dirty();
                        }
                    }
                    KeyCode::PageDown => {
                        if self.action_menu_selected + 1 < self.action_menu_items.len() {
                            let remaining =
                                self.action_menu_items.len() - self.action_menu_selected - 1;
                            let step = remaining.min(5);
                            self.action_menu_selected += step;
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('L') => {
                        let index = self
                            .action_menu_selected
                            .min(self.action_menu_items.len().saturating_sub(1));
                        let entry = self.action_menu_items[index].clone();
                        if !entry.enabled {
                            self.status_message =
                                "That action isn’t available right now.".to_string();
                            self.mark_dirty();
                            return Ok(false);
                        }
                        match entry.action {
                            ActionMenuAction::OpenLinks => {
                                let items = self.collect_links_for_current_context();
                                if items.is_empty() {
                                    self.status_message =
                                        "No links available in the current context.".to_string();
                                    self.mark_dirty();
                                } else {
                                    self.action_link_items = items;
                                    self.action_menu_mode = ActionMenuMode::Links;
                                    self.action_menu_selected = 0;
                                    self.status_message =
                                        "Links: j/k move · Enter/l open · h/Esc back".to_string();
                                    self.mark_dirty();
                                }
                            }
                            ActionMenuAction::SaveMedia => {
                                self.save_high_res_media()?;
                                self.action_menu_items = self.build_action_menu_entries();
                                if self.action_menu_selected >= self.action_menu_items.len() {
                                    self.action_menu_selected =
                                        self.action_menu_items.len().saturating_sub(1);
                                }
                            }
                            ActionMenuAction::StartVideo => {
                                self.needs_video_refresh = true;
                                match self.restart_inline_video() {
                                    Ok(()) => {
                                        self.close_action_menu(None);
                                        return Ok(false);
                                    }
                                    Err(err) => {
                                        self.status_message =
                                            format!("Unable to start inline video: {}", err);
                                        self.mark_dirty();
                                    }
                                }
                            }
                            ActionMenuAction::OpenVideoExternal => {
                                if self.pending_external_video.is_some() {
                                    self.status_message =
                                        "Video launch already in progress.".to_string();
                                    self.mark_dirty();
                                } else if self.banner_selected() {
                                    self.status_message =
                                        "Select a post before launching the video player."
                                            .to_string();
                                    self.mark_dirty();
                                } else if let Some(post) =
                                    self.posts.get(self.selected_post).cloned()
                                {
                                    let source = self
                                        .media_previews
                                        .get(&post.post.name)
                                        .and_then(|preview| {
                                            preview.video().map(|video| video.source.clone())
                                        })
                                        .or_else(|| video::find_video_source(&post.post));
                                    if let Some(source) = source {
                                        self.launch_external_video(source);
                                        self.close_action_menu(None);
                                        return Ok(false);
                                    } else {
                                        self.status_message =
                                            "No video available for the selected post.".to_string();
                                        self.mark_dirty();
                                    }
                                } else {
                                    self.status_message =
                                        "Select a post with a video before launching the player."
                                            .to_string();
                                    self.mark_dirty();
                                }
                            }
                            ActionMenuAction::StopVideo => {
                                if self.stop_active_video(Some("Video preview stopped."), false) {
                                    self.needs_video_refresh = false;
                                }
                                self.action_menu_items = self.build_action_menu_entries();
                                if self.action_menu_selected >= self.action_menu_items.len() {
                                    self.action_menu_selected =
                                        self.action_menu_items.len().saturating_sub(1);
                                }
                                self.close_action_menu(None);
                                return Ok(false);
                            }
                            ActionMenuAction::ToggleFullscreen => {
                                let was_fullscreen = self.media_fullscreen;
                                self.toggle_media_fullscreen()?;
                                if self.media_fullscreen != was_fullscreen {
                                    self.close_action_menu(None);
                                } else {
                                    self.action_menu_items = self.build_action_menu_entries();
                                    if self.action_menu_selected >= self.action_menu_items.len() {
                                        self.action_menu_selected =
                                            self.action_menu_items.len().saturating_sub(1);
                                    }
                                }
                            }
                            ActionMenuAction::ComposeComment => {
                                self.open_comment_composer()?;
                                if self.comment_composer.is_some() {
                                    self.close_action_menu(None);
                                    return Ok(false);
                                } else {
                                    self.action_menu_items = self.build_action_menu_entries();
                                    if self.action_menu_selected >= self.action_menu_items.len() {
                                        self.action_menu_selected =
                                            self.action_menu_items.len().saturating_sub(1);
                                    }
                                }
                            }
                            ActionMenuAction::OpenNavigation => {
                                self.open_navigation_mode(String::new(), true);
                                return Ok(false);
                            }
                        }
                    }
                    _ => {}
                }
            }
            ActionMenuMode::Links => {
                return self.handle_action_links_key(key);
            }
            ActionMenuMode::Navigation(mut state) => {
                if state.matches.is_empty() {
                    state.selected = 0;
                }

                match code {
                    KeyCode::Esc => {
                        if state.editing && !state.filter.is_empty() {
                            state.filter.clear();
                            self.refresh_navigation_matches(&mut state);
                            self.status_message =
                                "Command palette cleared. Type to search or Esc to close."
                                    .to_string();
                            self.mark_dirty();
                        } else {
                            self.close_action_menu(Some("Actions closed."));
                            return Ok(false);
                        }
                    }
                    KeyCode::Tab => {
                        state.editing = !state.editing;
                        if state.editing {
                            self.status_message =
                                "Command palette: typing enabled · Esc clear · Ctrl+H/J/K/L navigate"
                                    .to_string();
                        } else {
                            self.status_message =
                                "Command palette: browse with ↑/↓ · Enter/l open · h back · Ctrl+H/J/K/L navigate · Tab to type"
                                    .to_string();
                        }
                        self.mark_dirty();
                    }
                    KeyCode::Backspace => {
                        if state.editing && state.filter.pop().is_some() {
                            self.refresh_navigation_matches(&mut state);
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Delete => {
                        if state.editing && !state.filter.is_empty() {
                            state.filter.clear();
                            self.refresh_navigation_matches(&mut state);
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                        if state.editing && !state.filter.is_empty() {
                            state.filter.clear();
                            self.refresh_navigation_matches(&mut state);
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                        if state.editing && pop_last_word(&mut state.filter) {
                            self.refresh_navigation_matches(&mut state);
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(entry) = state.active_match().cloned() {
                            if !entry.enabled {
                                self.status_message =
                                    "That entry is unavailable right now.".to_string();
                                self.mark_dirty();
                            } else {
                                let filter_snapshot = state.filter.clone();
                                let editing_snapshot = state.editing;
                                self.activate_navigation_target(&entry.target)?;
                                if !self.action_menu_visible {
                                    return Ok(false);
                                }
                                state =
                                    self.build_navigation_state(filter_snapshot, editing_snapshot);
                            }
                        } else {
                            self.status_message = "No results to open.".to_string();
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Char('l') | KeyCode::Char('L') => {
                        if modifiers.contains(KeyModifiers::CONTROL) || !state.editing {
                            if let Some(entry) = state.active_match().cloned() {
                                if !entry.enabled {
                                    self.status_message =
                                        "That entry is unavailable right now.".to_string();
                                    self.mark_dirty();
                                } else {
                                    let filter_snapshot = state.filter.clone();
                                    let editing_snapshot = state.editing;
                                    self.activate_navigation_target(&entry.target)?;
                                    if !self.action_menu_visible {
                                        return Ok(false);
                                    }
                                    state = self
                                        .build_navigation_state(filter_snapshot, editing_snapshot);
                                }
                            } else {
                                self.status_message = "No results to open.".to_string();
                                self.mark_dirty();
                            }
                        } else {
                            let ch = match code {
                                KeyCode::Char('l') => 'l',
                                KeyCode::Char('L') => 'L',
                                _ => unreachable!(),
                            };
                            state.filter.push(ch);
                            self.refresh_navigation_matches(&mut state);
                            self.mark_dirty();
                        }
                    }
                    KeyCode::Up => {
                        state.select_previous_enabled();
                        self.mark_dirty();
                    }
                    KeyCode::Down => {
                        state.select_next_enabled();
                        self.mark_dirty();
                    }
                    KeyCode::PageUp => {
                        state.page_previous_enabled();
                        self.mark_dirty();
                    }
                    KeyCode::PageDown => {
                        state.page_next_enabled();
                        self.mark_dirty();
                    }
                    KeyCode::Char(ch)
                        if matches!(ch, 'h' | 'H')
                            && (modifiers.contains(KeyModifiers::CONTROL) || !state.editing) =>
                    {
                        self.action_menu_mode = ActionMenuMode::Root;
                        self.action_menu_items = self.build_action_menu_entries();
                        self.action_menu_selected = self
                            .action_menu_items
                            .iter()
                            .enumerate()
                            .find(|(_, entry)| entry.enabled)
                            .map(|(idx, _)| idx)
                            .unwrap_or(0);
                        self.status_message =
                            "Actions: j/k move · Ctrl+H/J/K/L navigate · Enter/l open · h/Esc/o back"
                                .to_string();
                        self.mark_dirty();
                        return Ok(false);
                    }
                    KeyCode::Char(ch)
                        if matches!(ch, 'k' | 'K')
                            && (modifiers.contains(KeyModifiers::CONTROL) || !state.editing) =>
                    {
                        state.select_previous_enabled();
                        self.mark_dirty();
                    }
                    KeyCode::Char(ch)
                        if matches!(ch, 'j' | 'J')
                            && (modifiers.contains(KeyModifiers::CONTROL) || !state.editing) =>
                    {
                        state.select_next_enabled();
                        self.mark_dirty();
                    }
                    KeyCode::Char(ch)
                        if state.editing
                            && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        state.filter.push(ch);
                        self.refresh_navigation_matches(&mut state);
                        self.mark_dirty();
                    }
                    _ => {}
                }

                self.action_menu_mode = ActionMenuMode::Navigation(state);
                return Ok(false);
            }
        }

        Ok(false)
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.close_help();
            }
            _ => {}
        }
        Ok(false)
    }

    fn draw_action_menu(&self, frame: &mut Frame<'_>, area: Rect) {
        match &self.action_menu_mode {
            ActionMenuMode::Root => self.draw_action_menu_root(frame, area),
            ActionMenuMode::Links => self.draw_action_menu_links(frame, area),
            ActionMenuMode::Navigation(state) => {
                self.draw_action_menu_navigation(frame, area, state)
            }
        }
    }

    fn draw_help_overlay(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup_area = centered_rect(80, 80, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(Span::styled(
                "Help",
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(COLOR_ACCENT))
            .style(Style::default().bg(COLOR_PANEL_BG))
            .padding(Padding::new(2, 2, 1, 1));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(2)])
            .split(inner);

        let sections = self.help_sections();
        let split_at = sections.len().div_ceil(2);
        let (left_sections, right_sections) = sections.split_at(split_at);

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(vertical[0]);

        let left_text = Self::help_column_text(left_sections);
        frame.render_widget(
            Paragraph::new(left_text)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(COLOR_TEXT_PRIMARY).bg(COLOR_PANEL_BG)),
            columns[0],
        );

        if !right_sections.is_empty() {
            let right_text = Self::help_column_text(right_sections);
            frame.render_widget(
                Paragraph::new(right_text)
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(COLOR_TEXT_PRIMARY).bg(COLOR_PANEL_BG)),
                columns[1],
            );
        }

        let footer = Paragraph::new("Press Esc or ? to close this overlay.")
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::ITALIC),
            );
        frame.render_widget(footer, vertical[1]);
    }

    fn draw_action_menu_root(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup_area = centered_rect(64, 52, area);
        frame.render_widget(Clear, popup_area);

        let items: Vec<ListItem> = if self.action_menu_items.is_empty() {
            vec![ListItem::new(vec![Line::from(Span::styled(
                "No actions available",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::ITALIC),
            ))])]
        } else {
            self.action_menu_items
                .iter()
                .map(|entry| {
                    let style = if entry.enabled {
                        Style::default()
                            .fg(COLOR_TEXT_PRIMARY)
                            .bg(COLOR_PANEL_BG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(COLOR_TEXT_SECONDARY)
                            .bg(COLOR_PANEL_BG)
                            .add_modifier(Modifier::ITALIC)
                    };
                    ListItem::new(vec![
                        Line::from(Span::styled(entry.label.clone(), style)),
                        Line::default(),
                    ])
                })
                .collect()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        "Actions",
                        Style::default()
                            .fg(COLOR_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(COLOR_ACCENT))
                    .style(Style::default().bg(COLOR_PANEL_BG))
                    .padding(Padding::new(2, 2, 1, 1)),
            )
            .highlight_style(
                Style::default()
                    .fg(COLOR_TEXT_PRIMARY)
                    .bg(COLOR_PANEL_SELECTED_BG)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(popup_area);

        let mut state = ListState::default();
        if !self.action_menu_items.is_empty() {
            state.select(Some(
                self.action_menu_selected
                    .min(self.action_menu_items.len().saturating_sub(1)),
            ));
        }

        frame.render_stateful_widget(list, chunks[0], &mut state);

        let instructions = Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::raw(
                "j/k move · Ctrl+H/J/K/L navigate · Enter/l open · h/Esc/o back",
            )),
        ])
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .bg(COLOR_PANEL_BG)
                .add_modifier(Modifier::ITALIC),
        );
        frame.render_widget(instructions, chunks[1]);
    }

    fn draw_action_menu_links(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup_area = centered_rect(70, 60, area);
        frame.render_widget(Clear, popup_area);

        let mut items: Vec<ListItem> = Vec::new();
        if self.action_link_items.is_empty() {
            items.push(ListItem::new(vec![Line::from(Span::styled(
                "No links available",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::ITALIC),
            ))]));
        } else {
            for entry in &self.action_link_items {
                let lines = vec![
                    Line::from(Span::styled(
                        entry.label.clone(),
                        Style::default()
                            .fg(COLOR_TEXT_PRIMARY)
                            .bg(COLOR_PANEL_BG)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        entry.url.clone(),
                        Style::default().fg(COLOR_ACCENT).bg(COLOR_PANEL_BG),
                    )),
                    Line::default(),
                ];
                items.push(ListItem::new(lines));
            }
        }

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        "Links",
                        Style::default()
                            .fg(COLOR_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(COLOR_ACCENT))
                    .style(Style::default().bg(COLOR_PANEL_BG)),
            )
            .highlight_style(
                Style::default()
                    .fg(COLOR_TEXT_PRIMARY)
                    .bg(COLOR_PANEL_SELECTED_BG)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(popup_area);

        let mut state = ListState::default();
        if !self.action_link_items.is_empty() {
            state.select(Some(
                self.action_menu_selected
                    .min(self.action_link_items.len().saturating_sub(1)),
            ));
        }

        frame.render_stateful_widget(list, chunks[0], &mut state);

        let instructions =
            Paragraph::new("j/k move · Ctrl+H/J/K/L navigate · Enter/l open · h/Ctrl+H/Esc back")
                .alignment(Alignment::Center)
                .style(
                    Style::default()
                        .fg(COLOR_TEXT_SECONDARY)
                        .bg(COLOR_PANEL_BG)
                        .add_modifier(Modifier::ITALIC),
                );
        frame.render_widget(instructions, chunks[1]);
    }

    fn draw_action_menu_navigation(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        state: &NavigationMenuState,
    ) {
        let popup_area = centered_rect(70, 60, area);
        frame.render_widget(Clear, popup_area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(popup_area);

        let prompt_style = if state.editing {
            Style::default()
                .fg(COLOR_ACCENT)
                .bg(COLOR_PANEL_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_TEXT_SECONDARY).bg(COLOR_PANEL_BG)
        };
        let prompt_label = if state.editing {
            "Search (typing):"
        } else {
            "Search:"
        };
        let filter_line = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(prompt_label, prompt_style),
                Span::raw(" "),
                Span::styled(
                    state.filter.as_str(),
                    Style::default()
                        .fg(COLOR_TEXT_PRIMARY)
                        .bg(COLOR_PANEL_BG)
                        .add_modifier(if state.editing {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]),
            Line::raw(""),
        ])
        .style(Style::default().bg(COLOR_PANEL_BG))
        .wrap(Wrap { trim: true });
        frame.render_widget(filter_line, layout[0]);

        let mut items: Vec<ListItem> = Vec::new();
        if state.matches.is_empty() {
            items.push(ListItem::new(vec![Line::from(Span::styled(
                "No matches",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::ITALIC),
            ))]));
        } else {
            for entry in &state.matches {
                let label_style = if entry.enabled {
                    Style::default()
                        .fg(COLOR_TEXT_PRIMARY)
                        .bg(COLOR_PANEL_BG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(COLOR_TEXT_SECONDARY)
                        .bg(COLOR_PANEL_BG)
                        .add_modifier(Modifier::ITALIC)
                };

                let mut lines = vec![Line::from(Span::styled(entry.label.clone(), label_style))];

                if let Some(description) = &entry.description {
                    lines.push(Line::from(Span::styled(
                        description.clone(),
                        Style::default()
                            .fg(COLOR_TEXT_SECONDARY)
                            .bg(COLOR_PANEL_BG)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                lines.push(Line::default());
                items.push(ListItem::new(lines));
            }
        }

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        "Navigation",
                        Style::default()
                            .fg(COLOR_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(COLOR_ACCENT))
                    .style(Style::default().bg(COLOR_PANEL_BG))
                    .padding(Padding::new(2, 2, 1, 1)),
            )
            .highlight_style(
                Style::default()
                    .fg(COLOR_TEXT_PRIMARY)
                    .bg(COLOR_PANEL_SELECTED_BG)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        let mut stateful = ListState::default();
        if !state.matches.is_empty() {
            let index = state.selected.min(state.matches.len().saturating_sub(1));
            stateful.select(Some(index));
        }
        frame.render_stateful_widget(list, layout[1], &mut stateful);

        let instructions = Paragraph::new(vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled(
                    "Type to search",
                    Style::default()
                        .fg(COLOR_TEXT_PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" · Tab toggle typing · Esc clear/close · n toggle NSFW"),
            ]),
            Line::from(vec![Span::raw(
                "Ctrl+H/J/K/L navigate even when typing · Enter/l open · h back",
            )]),
        ])
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .bg(COLOR_PANEL_BG)
                .add_modifier(Modifier::ITALIC),
        );
        frame.render_widget(instructions, layout[2]);
    }

    fn help_sections(&self) -> Vec<HelpSection> {
        let sections = vec![
            HelpSection::new(
                "Move around",
                vec![
                    ("h / l", "Focus the pane to the left or right"),
                    ("j / k", "Step through lists and menus"),
                    ("Ctrl+H/J/K/L", "Steer overlays even when typing"),
                    ("↑ / ↓", "Scroll within long views"),
                    ("Page↑ / Page↓ / Space", "Jump by a larger chunk"),
                ],
            ),
            HelpSection::new(
                "Open & switch",
                vec![
                    ("Enter", "Activate whatever is highlighted"),
                    ("o", "Open the actions menu"),
                    ("g", "Open the navigation palette"),
                    ("?", "Toggle this help overlay"),
                    ("m", "Open the guided setup menu"),
                    ("h / Esc", "Back out of menus"),
                ],
            ),
            HelpSection::new(
                "Refresh & sort",
                vec![
                    ("r", "Reload the current feed"),
                    ("s", "Refresh subscribed lists"),
                    ("t", "Focus comment sort controls"),
                    ("digits", "Jump directly to a post number"),
                ],
            ),
            HelpSection::new(
                "Vote & expand",
                vec![
                    ("u / d", "Upvote or downvote the selection"),
                    ("c", "Collapse or expand a comment thread"),
                    ("Shift+C", "Expand the comment thread fully"),
                ],
            ),
            HelpSection::new(
                "Extras",
                vec![
                    ("n", "Toggle NSFW posts on/off"),
                    ("y", "Copy the highlighted comment"),
                    (
                        "w",
                        "Write a comment (highlight the placeholder to post at the root)",
                    ),
                    ("Ctrl+S (composer)", "Submit the comment you are writing"),
                    ("f", "Toggle fullscreen media preview"),
                    ("space / p (video)", "Pause or resume inline playback"),
                    (
                        "[ / ] (video)",
                        "Seek inline video backward/forward 5 seconds",
                    ),
                    ("Esc (during video)", "Stop inline video playback"),
                    ("Esc (composer)", "Discard the comment draft"),
                    ("U", "Run the available updater"),
                    ("q / Esc", "Quit Reddix"),
                ],
            ),
        ];

        sections
    }

    fn help_column_text(sections: &[HelpSection]) -> Text<'static> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        for (index, section) in sections.iter().enumerate() {
            if index > 0 {
                lines.push(Line::default());
            }
            lines.push(Line::from(vec![Span::styled(
                section.title.clone(),
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::default());

            for (binding, description) in &section.entries {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<18}", binding),
                        Style::default()
                            .fg(COLOR_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(description.clone()),
                ]));
            }
        }

        Text::from(lines)
    }

    fn open_support_link(&mut self) -> Result<()> {
        match webbrowser::open(SUPPORT_LINK_URL) {
            Ok(_) => {
                self.status_message = "Opened support page in your browser.".to_string();
                self.mark_dirty();
                Ok(())
            }
            Err(err) => {
                let message = format!("Failed to open support page: {err}");
                self.status_message = message.clone();
                self.mark_dirty();
                Err(anyhow!(message))
            }
        }
    }

    fn open_project_link(&mut self) -> Result<()> {
        match webbrowser::open(PROJECT_LINK_URL) {
            Ok(_) => {
                self.status_message = "Opened project page on GitHub.".to_string();
                self.mark_dirty();
                Ok(())
            }
            Err(err) => {
                let message = format!("Failed to open project page: {err}");
                self.status_message = message.clone();
                self.mark_dirty();
                Err(anyhow!(message))
            }
        }
    }

    fn open_release_notes_link(&mut self) -> Result<()> {
        let Some(note) = &self.release_note else {
            self.status_message = "Release notes unavailable.".to_string();
            self.mark_dirty();
            return Ok(());
        };

        match webbrowser::open(note.release_url.as_str()) {
            Ok(_) => {
                self.release_note_unread = false;
                self.status_message = format!(
                    "Opened release notes for v{} in your browser.",
                    note.version
                );
                self.mark_dirty();
                Ok(())
            }
            Err(err) => {
                let message = format!("Failed to open release notes: {err}");
                self.status_message = message.clone();
                self.mark_dirty();
                Err(anyhow!(message))
            }
        }
    }

    fn open_menu(&mut self) -> Result<()> {
        self.menu_form = MenuForm::default();
        self.menu_screen = MenuScreen::Accounts;
        self.queue_active_kitty_delete();
        self.close_action_menu(None);
        self.menu_visible = true;

        let status = match self.refresh_menu_accounts() {
            Ok(_) => {
                if let Some(pos) = self.menu_accounts.iter().position(|entry| entry.is_active) {
                    self.menu_account_index = pos;
                } else {
                    self.menu_account_index = 0;
                }
                if self.menu_accounts.is_empty() {
                    "Guided menu: no Reddit accounts found. Press a to add one.".to_string()
                } else {
                    "Guided menu: j/k select account · Enter switch · a add · Esc/m close"
                        .to_string()
                }
            }
            Err(err) => {
                self.menu_accounts.clear();
                self.menu_account_index = 0;
                format!("Guided menu: failed to list accounts: {}", err)
            }
        };

        self.status_message = status;
        self.mark_dirty();
        Ok(())
    }

    fn ensure_session_manager(&mut self) -> Result<Arc<session::Manager>> {
        if let Some(manager) = &self.session_manager {
            return Ok(manager.clone());
        }

        let mut cfg = config::load(config::LoadOptions::default()).context("load config")?;
        if cfg.reddit.client_id.trim().is_empty() {
            bail!("Reddit client ID is required before starting authorization");
        }
        if cfg.reddit.user_agent.trim().is_empty() {
            cfg.reddit.user_agent = config::RedditConfig::default().user_agent;
        }
        if cfg.reddit.scopes.is_empty() {
            cfg.reddit.scopes = config::RedditConfig::default().scopes;
        }

        let flow_cfg = auth::Config {
            client_id: cfg.reddit.client_id.clone(),
            client_secret: cfg.reddit.client_secret.clone(),
            scope: cfg.reddit.scopes.clone(),
            user_agent: cfg.reddit.user_agent.clone(),
            auth_url: "https://www.reddit.com/api/v1/authorize".into(),
            token_url: "https://www.reddit.com/api/v1/access_token".into(),
            identity_url: "https://oauth.reddit.com/api/v1/me".into(),
            redirect_uri: cfg.reddit.redirect_uri.clone(),
            refresh_skew: Duration::from_secs(30),
        };

        let flow =
            Arc::new(auth::Flow::new(self.store.clone(), flow_cfg).context("create auth flow")?);
        let manager = Arc::new(
            session::Manager::new(self.store.clone(), flow).context("create session manager")?,
        );
        self.session_manager = Some(manager.clone());
        Ok(manager)
    }

    fn start_authorization_flow(&mut self, saved_path: &Path) -> Result<()> {
        let manager = self.ensure_session_manager()?;
        let authz = manager
            .begin_login()
            .context("start Reddit authorization")?;
        let url = authz.browser_url.clone();

        self.login_in_progress = true;
        self.menu_form.authorization_started(url.clone());

        let mut message = format!("Saved Reddit credentials to {}. ", saved_path.display());

        match webbrowser::open(&url) {
            Ok(_) => {
                message.push_str(
                    "Authorize Reddix in your browser, then return here once it finishes. If nothing opens automatically, use Open Link below.",
                );
            }
            Err(err) => {
                message.push_str(&format!(
                    "Open {} in your browser to authorize (auto-open failed: {}).",
                    url, err
                ));
            }
        }

        self.menu_form.set_status(message.clone());
        self.status_message = message;

        let tx = self.response_tx.clone();
        let manager_clone = manager.clone();
        thread::spawn(move || {
            let result = manager_clone
                .complete_login(authz)
                .map(|session| session.account.username);
            let _ = tx.send(AsyncResponse::Login { result });
        });

        self.mark_dirty();
        Ok(())
    }

    fn open_auth_link_in_browser(&mut self) -> Result<()> {
        let Some(url) = self.menu_form.auth_link().map(|s| s.to_string()) else {
            bail!("authorization link unavailable");
        };
        webbrowser::open(&url).map_err(|err| anyhow!("open authorization link: {}", err))?;
        let message = "Authorization link opened in your browser.".to_string();
        self.menu_form.set_status(message.clone());
        self.status_message = message;
        self.mark_dirty();
        Ok(())
    }

    fn setup_authenticated_services(&mut self) -> Result<()> {
        let manager = self.ensure_session_manager()?;
        let cfg = config::load(config::LoadOptions::default()).context("load config")?;
        let user_agent = if cfg.reddit.user_agent.trim().is_empty() {
            config::RedditConfig::default().user_agent
        } else {
            cfg.reddit.user_agent.clone()
        };
        let token_provider = manager
            .active_token_provider()
            .context("retrieve active Reddit session")?;
        let client = Arc::new(
            reddit::Client::new(
                token_provider,
                reddit::ClientConfig {
                    user_agent,
                    base_url: None,
                    http_client: None,
                },
            )
            .context("create reddit client")?,
        );

        self.feed_service = Some(Arc::new(crate::data::RedditFeedService::new(
            client.clone(),
        )));
        self.subreddit_service = Some(Arc::new(crate::data::RedditSubredditService::new(
            client.clone(),
        )));
        self.comment_service = Some(Arc::new(crate::data::RedditCommentService::new(
            client.clone(),
        )));
        self.interaction_service =
            Some(Arc::new(crate::data::RedditInteractionService::new(client)));
        Ok(())
    }

    fn handle_login_success(&mut self, username: String) -> Result<()> {
        self.menu_form.authorization_complete();
        let message = format!(
            "Authorization complete. Signed in as {}. Loading Reddit data...",
            username
        );
        self.menu_form.set_status(message.clone());
        self.status_message = message;

        self.setup_authenticated_services()?;
        self.ensure_cache_scope();

        if let Err(err) = self.reload_subreddits() {
            let msg = format!("Failed to refresh subreddits: {err}");
            self.menu_form.set_status(msg.clone());
            self.status_message = msg;
        }
        if let Err(err) = self.reload_posts() {
            let msg = format!("Failed to reload posts: {err}");
            self.menu_form.set_status(msg.clone());
            self.status_message = msg;
        }

        if let Err(err) = self.refresh_menu_accounts() {
            self.status_message = format!(
                "Signed in as {}, but failed to refresh account list: {}",
                username, err
            );
        } else {
            self.menu_screen = MenuScreen::Accounts;
            if let Some(pos) = self.menu_accounts.iter().position(|entry| entry.is_active) {
                self.menu_account_index = pos;
            } else if !self.menu_accounts.is_empty() {
                self.menu_account_index = 0;
            }
        }

        if let Some(account_id) = self.active_account_id() {
            self.join_states.entry(account_id).or_default();
            self.queue_join_status_check();
        }

        self.mark_dirty();
        Ok(())
    }

    fn poll_async(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.response_rx.try_recv() {
            self.handle_async_response(message);
            changed = true;
        }
        changed
    }

    fn handle_async_response(&mut self, message: AsyncResponse) {
        match message {
            AsyncResponse::Posts {
                request_id,
                target,
                sort,
                result,
            } => {
                let Some(pending) = &self.pending_posts else {
                    return;
                };
                if pending.cancel_flag.load(Ordering::SeqCst) {
                    return;
                }
                if pending.request_id != request_id {
                    return;
                }
                let mode = pending.mode;
                self.pending_posts = None;
                if matches!(mode, LoadMode::Replace) {
                    self.pending_comments = None;
                }

                match result {
                    Ok(batch) => {
                        let key = FeedCacheKey::new(&target, sort);
                        self.apply_posts_batch(&target, sort, batch, false, mode);
                        if !self.posts.is_empty() {
                            let snapshot = PostBatch {
                                posts: self.posts.clone(),
                                after: self.feed_after.clone(),
                            };
                            self.cache_posts(key, snapshot);
                        }
                    }
                    Err(err) => {
                        self.status_message = format!("Failed to load posts: {err}");
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::PostRows {
                request_id,
                width,
                rows,
            } => {
                let Some(pending) = &self.pending_post_rows else {
                    return;
                };
                if pending.request_id != request_id || pending.width != width {
                    return;
                }
                self.pending_post_rows = None;
                if self.post_rows_width != width {
                    self.post_rows.clear();
                    self.post_rows_width = width;
                }
                for (name, data) in rows {
                    self.post_rows.insert(name, data);
                }
                if let Some(selected) = self.posts.get(self.selected_post) {
                    let key = selected.post.name.clone();
                    if self.content_cache.contains_key(&key) && self.post_rows.contains_key(&key) {
                        self.sync_content_from_selection();
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::Comments {
                request_id,
                post_name,
                sort,
                result,
            } => {
                let Some(pending) = &self.pending_comments else {
                    return;
                };
                if pending.cancel_flag.load(Ordering::SeqCst)
                    || pending.request_id != request_id
                    || pending.post_name != post_name
                    || pending.sort != sort
                {
                    return;
                }
                let current_name = self
                    .posts
                    .get(self.selected_post)
                    .map(|post| post.post.name.as_str());
                if current_name != Some(post_name.as_str()) || self.comment_sort != sort {
                    return;
                }
                self.pending_comments = None;

                match result {
                    Ok(comments) => {
                        self.cache_comments(&post_name, sort, comments.clone());
                        self.comments = comments;
                        self.insert_post_root_comment_placeholder();
                        self.collapsed_comments.clear();
                        self.selected_comment = 0;
                        self.comment_offset.set(0);
                        self.rebuild_visible_comments_reset();
                        self.recompute_comment_status();
                    }
                    Err(err) => {
                        self.comments.clear();
                        self.collapsed_comments.clear();
                        self.visible_comment_indices.clear();
                        self.selected_comment = 0;
                        self.comment_offset.set(0);
                        self.comment_status = format!("Failed to load comments: {err}");
                    }
                }
                self.close_action_menu(None);
                self.mark_dirty();
            }
            AsyncResponse::Content {
                request_id,
                post_name,
                rendered,
            } => {
                let Some(pending) = &self.pending_content else {
                    return;
                };
                if pending.request_id != request_id || pending.post_name != post_name {
                    return;
                }
                if pending.cancel_flag.load(Ordering::SeqCst) {
                    return;
                }
                self.pending_content = None;
                let cache_entry = rendered.clone();
                self.content_cache.insert(post_name.clone(), cache_entry);
                let target_post = self
                    .posts
                    .iter()
                    .find(|candidate| candidate.post.name == post_name)
                    .cloned();
                if let Some(ref post) = target_post {
                    self.content = self.compose_content(rendered.clone(), post);
                    self.ensure_media_request_ready(post);
                } else {
                    self.content = rendered;
                }
                self.mark_dirty();
            }
            AsyncResponse::Subreddits { request_id, result } => {
                let Some(pending) = &self.pending_subreddits else {
                    return;
                };
                if pending.request_id != request_id {
                    return;
                }
                self.pending_subreddits = None;

                match result {
                    Ok(names) => {
                        let previous = self
                            .subreddits
                            .get(self.selected_sub)
                            .cloned()
                            .unwrap_or_else(|| "r/frontpage".to_string());

                        let custom_targets: Vec<String> = self
                            .subreddits
                            .iter()
                            .filter(|name| {
                                matches!(
                                    classify_feed_target(name),
                                    FeedKind::User(_) | FeedKind::Search(_)
                                )
                            })
                            .cloned()
                            .collect();

                        self.subreddits = names
                            .into_iter()
                            .map(|name| normalize_subreddit_name(&name))
                            .collect();
                        ensure_core_subreddits(&mut self.subreddits);

                        for extra in custom_targets {
                            if !self
                                .subreddits
                                .iter()
                                .any(|candidate| candidate.eq_ignore_ascii_case(&extra))
                            {
                                self.subreddits.push(extra);
                            }
                        }

                        if let Some(idx) = self
                            .subreddits
                            .iter()
                            .position(|candidate| candidate.eq_ignore_ascii_case(previous.as_str()))
                        {
                            self.selected_sub = idx;
                        } else if self.selected_sub >= self.subreddits.len() {
                            self.selected_sub = 0;
                        }
                        self.nav_index = self
                            .selected_sub
                            .min(self.subreddits.len().saturating_sub(1));
                        self.nav_mode = NavMode::Subreddits;
                        self.ensure_subreddit_visible();
                        self.status_message = "Subreddits refreshed".to_string();
                        if let Err(err) = self.reload_posts() {
                            self.status_message = format!("Failed to reload posts: {err}");
                        }
                    }
                    Err(err) => {
                        self.status_message = format!("Failed to refresh subreddits: {err}");
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::Media { post_name, result } => {
                self.pending_media.remove(&post_name);
                self.remove_pending_media_tracking(&post_name);
                let relevant = self.posts.iter().any(|post| post.post.name == post_name);
                if !relevant {
                    return;
                }
                match result {
                    Ok(MediaLoadOutcome::Ready(preview)) => {
                        let has_video = preview.video().is_some();
                        self.media_failures.remove(&post_name);
                        self.media_previews.insert(post_name.clone(), preview);
                        if has_video && self.active_video.is_some() {
                            self.needs_video_refresh = true;
                        }
                    }
                    Ok(MediaLoadOutcome::Absent) => {
                        self.media_failures.insert(post_name.clone());
                        self.media_previews.remove(&post_name);
                        if self.active_video.is_some() {
                            let _ = self.stop_active_video(None, true);
                        }
                    }
                    Ok(MediaLoadOutcome::Deferred) => {
                        if self.active_video.is_some() {
                            let _ = self.stop_active_video(None, true);
                        }
                    }
                    Err(err) => {
                        self.media_failures.insert(post_name.clone());
                        self.media_previews.remove(&post_name);
                        if self.active_video.is_some() {
                            let _ = self.stop_active_video(None, true);
                        }
                        self.status_message = format!("Image preview failed: {}", err);
                    }
                }

                let current = self
                    .posts
                    .get(self.selected_post)
                    .map(|post| post.post.name.as_str());
                if current == Some(post_name.as_str()) {
                    self.sync_content_from_selection();
                }
                self.mark_dirty();
            }
            AsyncResponse::InlineVideo {
                request_id,
                post_name,
                result,
            } => {
                let Some(pending) = self.pending_video.as_ref() else {
                    return;
                };
                if pending.request_id != request_id || pending.post_name != post_name {
                    return;
                }
                if pending.cancel_flag.load(Ordering::SeqCst) {
                    self.pending_video = None;
                    return;
                }
                let current = self
                    .posts
                    .get(self.selected_post)
                    .map(|post| post.post.name.as_str());
                if current != Some(post_name.as_str()) {
                    self.pending_video = None;
                    return;
                }
                let pending = self.pending_video.take().unwrap();
                let PendingVideo {
                    source,
                    origin,
                    dims,
                    ..
                } = pending;
                match result {
                    Ok(path) => {
                        video::debug_log(format!(
                            "video cache hit url={} path={}",
                            source.playback_url, path
                        ));
                        if let Err(err) = self.launch_inline_video(
                            post_name.clone(),
                            origin,
                            source,
                            dims,
                            path.clone(),
                            false,
                        ) {
                            self.status_message = format!("Failed to start video preview: {}", err);
                            self.mark_dirty();
                        }
                    }
                    Err(err) => {
                        let playback_url = source.playback_url.clone();
                        video::debug_log(format!(
                            "video cache fetch failed for {}: {}",
                            playback_url, err
                        ));
                        if let Err(play_err) = self.launch_inline_video(
                            post_name.clone(),
                            origin,
                            source,
                            dims,
                            playback_url.clone(),
                            true,
                        ) {
                            self.status_message =
                                format!("Failed to start video preview: {}", play_err);
                            self.mark_dirty();
                        } else {
                            self.status_message
                                .push_str(" Cache unavailable; streaming directly.");
                            self.mark_dirty();
                        }
                    }
                }
            }
            AsyncResponse::ExternalVideo {
                request_id,
                label,
                result,
            } => {
                if self.pending_external_video != Some(request_id) {
                    return;
                }
                self.pending_external_video = None;
                match result {
                    Ok(()) => {
                        self.status_message =
                            format!("Launched fullscreen player for \"{}\".", label);
                    }
                    Err(err) => {
                        self.status_message = format!(
                            "Failed to launch fullscreen player for \"{}\": {}",
                            label, err
                        );
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::MediaSave { result } => {
                self.media_save_in_progress = None;
                if self.action_menu_visible {
                    self.action_menu_items = self.build_action_menu_entries();
                    if self.action_menu_selected >= self.action_menu_items.len() {
                        self.action_menu_selected = self.action_menu_items.len().saturating_sub(1);
                    }
                }

                match result {
                    Ok(outcome) => {
                        let count = outcome.saved_paths.len();
                        self.status_message = if count == 1 {
                            outcome
                                .saved_paths
                                .first()
                                .map(|path| format!("Saved media to {}", path.display()))
                                .unwrap_or_else(|| "Saved media.".to_string())
                        } else {
                            format!("Saved {} files to {}", count, outcome.dest_dir.display())
                        };
                    }
                    Err(err) => {
                        self.status_message = format!("Failed to save media: {}", err);
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::Login { result } => {
                self.login_in_progress = false;
                match result {
                    Ok(username) => {
                        if let Err(err) = self.handle_login_success(username) {
                            let message = format!(
                                "Authorization completed but initializing Reddit client failed: {}",
                                err
                            );
                            self.menu_form.authorization_complete();
                            self.menu_form.set_status(message.clone());
                            self.status_message = message;
                        }
                    }
                    Err(err) => {
                        self.menu_form.authorization_complete();
                        let message = format!("Authorization failed: {}", err);
                        self.menu_form.set_status(message.clone());
                        self.status_message = message;
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::Update { result } => {
                self.update_check_in_progress = false;
                self.update_checked = true;
                match result {
                    Ok(Some(info)) => {
                        self.latest_known_version = Some(info.version.clone());
                        self.update_notice = Some(info.clone());
                        self.update_banner_selected = true;
                        self.update_install_finished = false;
                        self.status_message = format!(
                            "Update {} available — press Enter or Shift+U to install now.",
                            info.version
                        );
                    }
                    Ok(None) => {
                        self.update_notice = None;
                        self.latest_known_version = Some(self.current_version.clone());
                        self.update_banner_selected = false;
                        self.update_install_finished = false;
                    }
                    Err(err) => {
                        self.update_notice = None;
                        self.update_banner_selected = false;
                        self.update_install_finished = false;
                        let mut message = format!("Update check failed: {}", err);
                        if let Some(network_err) = err.downcast_ref::<ReqwestError>() {
                            if network_err.is_connect() || network_err.is_timeout() {
                                message = "Update check skipped: network unavailable.".to_string();
                            }
                        }
                        self.status_message = message;
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::UpdateInstall { result } => {
                self.update_install_in_progress = false;
                match result {
                    Ok(()) => {
                        if let Some(update) = &self.update_notice {
                            self.status_message = format!(
                                "Update v{} installed. Restart Reddix to finish applying it.",
                                update.version
                            );
                        } else {
                            self.status_message =
                                "Update installed. Restart Reddix to finish applying it."
                                    .to_string();
                        }
                        self.update_banner_selected = false;
                        self.update_install_finished = true;
                    }
                    Err(err) => {
                        self.status_message = format!("Update install failed: {}", err);
                        self.update_banner_selected = false;
                        self.update_install_finished = false;
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::KittyProbe { result } => {
                self.handle_kitty_probe(result);
            }
            AsyncResponse::JoinStatus { account_id, result } => {
                let state = self.join_states.entry(account_id).or_default();
                match result {
                    Ok(joined) => {
                        state.pending = false;
                        state.joined = joined;
                        state.last_error = None;
                    }
                    Err(err) => {
                        state.mark_error(err.to_string());
                        self.status_message = format!(
                            "Checking {} subscription failed: {}",
                            REDDIX_COMMUNITY_DISPLAY, err
                        );
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::JoinCommunity { account_id, result } => {
                let state = self.join_states.entry(account_id).or_default();
                state.pending = false;
                match result {
                    Ok(()) => {
                        state.mark_success();
                        self.status_message = format!(
                            "Joined {}. Thanks for supporting the community!",
                            REDDIX_COMMUNITY_DISPLAY
                        );
                    }
                    Err(err) => {
                        let message =
                            format!("Joining {} failed: {}", REDDIX_COMMUNITY_DISPLAY, err);
                        state.mark_error(message.clone());
                        self.status_message = message;
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::CommentSubmit { request_id, result } => {
                let Some(pending) = self.pending_comment_submit.take() else {
                    return;
                };
                if pending.request_id != request_id {
                    self.pending_comment_submit = Some(pending);
                    return;
                }
                let PendingCommentSubmit {
                    post_fullname,
                    target,
                    ..
                } = pending;
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.submitting = false;
                }

                match result {
                    Ok(comment) => {
                        self.status_message = "Comment posted.".to_string();
                        self.insert_posted_comment(comment, target);
                        self.comment_composer = None;
                        self.comment_cache
                            .retain(|key, _| key.post_name != post_fullname);
                    }
                    Err(err) => {
                        let err_text = err.to_string();
                        let mut message = format!("Failed to submit comment: {}", err_text);
                        if err_text.to_lowercase().contains("forbidden") {
                            message.push_str(
                                " (Reddit rejected the request — ensure reddit.scopes includes \"submit\" and re-authorize if needed.)",
                            );
                        }
                        if let Some(composer) = self.comment_composer.as_mut() {
                            composer.set_status(message.clone());
                        }
                        self.status_message = message;
                    }
                }
                self.mark_dirty();
            }
            AsyncResponse::VoteResult {
                target,
                requested,
                previous,
                error,
            } => {
                let action_word = match requested {
                    1 => ("Upvoted", "upvote"),
                    -1 => ("Downvoted", "downvote"),
                    _ => ("Cleared vote on", "clear vote on"),
                };
                match target {
                    VoteTarget::Post { fullname } => {
                        if let Some(post) = self
                            .posts
                            .iter_mut()
                            .find(|candidate| candidate.post.name == fullname)
                        {
                            let current_vote = vote_from_likes(post.post.likes);
                            if let Some(err) = error {
                                if current_vote == requested {
                                    if requested != previous {
                                        post.post.score += (previous - requested) as i64;
                                    }
                                    post.post.likes = likes_from_vote(previous);
                                    self.post_rows.remove(&fullname);
                                }
                                self.status_message = format!(
                                    "Failed to {} \"{}\": {}",
                                    action_word.1, post.post.title, err
                                );
                            } else {
                                self.status_message =
                                    format!("{} \"{}\".", action_word.0, post.post.title);
                                self.post_rows.remove(&fullname);
                            }
                            self.mark_dirty();
                        }
                    }
                    VoteTarget::Comment { fullname } => {
                        let mut cache_update = None;
                        if let Some((index, comment)) = self
                            .comments
                            .iter_mut()
                            .enumerate()
                            .find(|(_, entry)| entry.name == fullname)
                        {
                            let current_vote = vote_from_likes(comment.likes);
                            if let Some(err) = error {
                                if current_vote == requested {
                                    if requested != previous {
                                        comment.score += (previous - requested) as i64;
                                    }
                                    comment.likes = likes_from_vote(previous);
                                }
                                self.status_message = format!(
                                    "Failed to {} comment by u/{}: {}",
                                    action_word.1, comment.author, err
                                );
                            } else {
                                self.status_message =
                                    format!("{} comment by u/{}.", action_word.0, comment.author);
                                comment.score_hidden = false;
                            }
                            cache_update = Some((index, comment.score, comment.likes));
                            self.ensure_comment_visible();
                            self.mark_dirty();
                        }
                        if let Some((index, score, likes)) = cache_update {
                            if let Some(post_name) = self
                                .posts
                                .get(self.selected_post)
                                .map(|post| post.post.name.clone())
                            {
                                let key = CommentCacheKey::new(&post_name, self.comment_sort);
                                if let Some(cache) = self.scoped_comment_cache_mut(&key) {
                                    if let Some(entry) = cache.comments.get_mut(index) {
                                        entry.score = score;
                                        entry.likes = likes;
                                        entry.score_hidden = false;
                                    }
                                    cache.fetched_at = Instant::now();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn navigate_in_focus(&mut self, delta: i32) -> Result<()> {
        match self.focused_pane {
            Pane::Navigation => match self.nav_mode {
                NavMode::Sorts => {
                    if delta > 0 && !self.subreddits.is_empty() {
                        self.nav_mode = NavMode::Subreddits;
                        self.nav_index = self
                            .selected_sub
                            .min(self.subreddits.len().saturating_sub(1));
                        self.ensure_subreddit_visible();
                        self.status_message =
                            "Use j/k inside the list, Enter to load; press k on the first subreddit to return to sort.".to_string();
                    }
                }
                NavMode::Subreddits => {
                    if self.subreddits.is_empty() {
                        return Ok(());
                    }

                    let len = self.subreddits.len() as i32;
                    let current = self.nav_index as i32;
                    let next = (current + delta).clamp(0, len.saturating_sub(1));

                    if delta < 0 && current == 0 && next == 0 {
                        self.nav_mode = NavMode::Sorts;
                        self.status_message = format!(
                            "Sort row selected ({sort}). Use ←/→ or 1-5 to change, Enter reloads.",
                            sort = sort_label(self.sort)
                        );
                    } else if next != current {
                        self.nav_index = next as usize;
                        self.ensure_subreddit_visible();
                        if let Some(name) = self.subreddits.get(self.nav_index) {
                            self.status_message = format!(
                                "Highlighted {} · {} — press Enter to load.",
                                name,
                                sort_label(self.sort)
                            );
                        }
                    }
                }
            },
            Pane::Posts => {
                let len = self.post_list_len();
                if len == 0 {
                    return Ok(());
                }
                let len = len as i32;
                let current = self.current_post_cursor() as i32;
                let next = (current + delta).clamp(0, len.saturating_sub(1));
                if next != current {
                    self.set_post_cursor(next as usize);
                }
                if self.banner_selected() {
                    self.selected_post = self.selected_post.min(self.posts.len().saturating_sub(1));
                }
            }
            Pane::Content => {
                if delta > 0 {
                    self.content_scroll = self.content_scroll.saturating_add(delta as u16);
                } else {
                    let magnitude = (-delta) as u16;
                    self.content_scroll = self.content_scroll.saturating_sub(magnitude);
                }
                if self.selected_post_has_inline_media() {
                    self.needs_kitty_flush = true;
                }
            }
            Pane::Comments => {
                if self.comment_sort_selected {
                    if delta > 0 {
                        self.comment_sort_selected = false;
                        self.mark_dirty();
                    }
                    return Ok(());
                }
                if self.visible_comment_indices.is_empty() {
                    return Ok(());
                }
                let len = self.visible_comment_indices.len() as i32;
                let current = self.selected_comment as i32;
                let next = (current + delta).clamp(0, len.saturating_sub(1));
                if next == current && delta < 0 && current == 0 {
                    self.comment_sort_selected = true;
                    self.status_message = format!(
                        "Comment sort selected ({}). Use ←/→ or 1-{} to change; j/k returns to comments.",
                        comment_sort_label(self.comment_sort),
                        COMMENT_SORTS.len()
                    );
                    self.mark_dirty();
                    return Ok(());
                }
                if next != current {
                    self.selected_comment = next as usize;
                    self.ensure_comment_visible();
                }
            }
        }
        Ok(())
    }

    fn vote_selected_post(&mut self, dir: i32) {
        let service = match self.interaction_service.as_ref() {
            Some(service) => Arc::clone(service),
            None => {
                self.status_message =
                    "Voting requires a signed-in Reddit session (press m to log in).".to_string();
                return;
            }
        };

        if self.posts.is_empty() {
            self.status_message = "No posts available to vote on.".to_string();
        } else {
            let index = self.selected_post.min(self.posts.len().saturating_sub(1));
            let fullname = self.posts[index].post.name.clone();
            if fullname.is_empty() {
                self.status_message = "Unable to vote on this post.".to_string();
                return;
            }
            let title = self.posts[index].post.title.clone();
            let action_word = match dir {
                1 => "Upvoted",
                -1 => "Downvoted",
                _ => "Cleared vote on",
            };
            let new_vote = dir.clamp(-1, 1);
            let old_vote = if let Some(post) = self.posts.get(index) {
                vote_from_likes(post.post.likes)
            } else {
                0
            };
            if let Some(post) = self.posts.get_mut(index) {
                if old_vote != new_vote {
                    post.post.score += (new_vote - old_vote) as i64;
                }
                post.post.likes = likes_from_vote(new_vote);
            }
            self.post_rows.remove(&fullname);
            self.status_message = format!("{} \"{}\" (sending...)", action_word, title);
            self.mark_dirty();

            let tx = self.response_tx.clone();
            let requested = new_vote;
            let previous = old_vote;
            thread::spawn(move || {
                let error = service
                    .vote(fullname.as_str(), dir)
                    .err()
                    .map(|err| err.to_string());
                let _ = tx.send(AsyncResponse::VoteResult {
                    target: VoteTarget::Post {
                        fullname: fullname.clone(),
                    },
                    requested,
                    previous,
                    error,
                });
            });
        }
    }

    fn vote_selected_comment(&mut self, dir: i32) {
        let service = match self.interaction_service.as_ref() {
            Some(service) => Arc::clone(service),
            None => {
                self.status_message =
                    "Voting requires a signed-in Reddit session (press m to log in).".to_string();
                return;
            }
        };

        if self.visible_comment_indices.is_empty() {
            self.status_message = "No comments available to vote on.".to_string();
            return;
        }

        let visible_len = self.visible_comment_indices.len();
        let selection = self.selected_comment.min(visible_len.saturating_sub(1));
        let comment_index = match self.visible_comment_indices.get(selection) {
            Some(index) => *index,
            None => {
                self.status_message = "Comment selection is out of sync.".to_string();
                return;
            }
        };

        let (fullname, author, is_root) = match self.comments.get(comment_index) {
            Some(comment) => (
                comment.name.clone(),
                comment.author.clone(),
                comment.is_post_root,
            ),
            None => {
                self.status_message = "Comment selection is out of sync.".to_string();
                return;
            }
        };

        if is_root {
            self.status_message = "Top-level placeholder cannot be voted on.".to_string();
            return;
        }

        if fullname.is_empty() {
            self.status_message = "Unable to vote on this comment.".to_string();
            return;
        }
        let action_word = match dir {
            1 => "Upvoted",
            -1 => "Downvoted",
            _ => "Cleared vote on",
        };

        let new_vote = dir.clamp(-1, 1);
        let old_vote = if let Some(entry) = self.comments.get(comment_index) {
            vote_from_likes(entry.likes)
        } else {
            0
        };

        let mut updated_comment = None;
        if let Some(entry) = self.comments.get_mut(comment_index) {
            if old_vote != new_vote {
                entry.score += (new_vote - old_vote) as i64;
            }
            entry.likes = likes_from_vote(new_vote);
            entry.score_hidden = false;
            updated_comment = Some((entry.score, entry.likes));
        }

        if let Some((score, likes)) = updated_comment {
            if let Some(post_name) = self
                .posts
                .get(self.selected_post)
                .map(|post| post.post.name.clone())
            {
                let key = CommentCacheKey::new(&post_name, self.comment_sort);
                let root_offset = if self
                    .comments
                    .first()
                    .is_some_and(|entry| entry.is_post_root)
                {
                    1
                } else {
                    0
                };
                let cache_index = comment_index.saturating_sub(root_offset);
                if let Some(cache) = self.scoped_comment_cache_mut(&key) {
                    if let Some(entry) = cache.comments.get_mut(cache_index) {
                        entry.score = score;
                        entry.likes = likes;
                        entry.score_hidden = false;
                    }
                    cache.fetched_at = Instant::now();
                }
            }
        }

        self.ensure_comment_visible();
        self.status_message = format!("{} comment by u/{} (sending...)", action_word, author);
        self.mark_dirty();

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let error = service
                .vote(fullname.as_str(), dir)
                .err()
                .map(|err| err.to_string());
            let _ = tx.send(AsyncResponse::VoteResult {
                target: VoteTarget::Comment {
                    fullname: fullname.clone(),
                },
                requested: new_vote,
                previous: old_vote,
                error,
            });
        });
    }

    fn copy_selected_comment(&mut self) -> Result<()> {
        let Some(comment_index) = self.selected_comment_index() else {
            self.status_message = "Select a comment to copy first.".to_string();
            self.mark_dirty();
            return Ok(());
        };

        let (text, author_label) = match self.comments.get(comment_index) {
            Some(comment) => {
                if comment.is_post_root {
                    self.status_message = "Highlight a real comment before copying.".to_string();
                    self.mark_dirty();
                    return Ok(());
                }
                let cleaned = comment.raw_body.trim_end().to_string();
                let label = if comment.author.trim().is_empty() {
                    "[deleted]".to_string()
                } else {
                    format!("u/{}", comment.author.trim())
                };
                (cleaned, label)
            }
            None => {
                self.status_message = "Comment selection is out of sync.".to_string();
                self.mark_dirty();
                return Ok(());
            }
        };

        if text.trim().is_empty() {
            self.status_message = "Selected comment has no text to copy.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        copy_to_clipboard(&mut self.clipboard, &text)?;
        self.status_message = format!("Copied comment by {} to the clipboard.", author_label);
        self.mark_dirty();
        Ok(())
    }

    fn ellipsize_label(text: &str, max_width: usize) -> String {
        if UnicodeWidthStr::width(text) <= max_width {
            return text.to_string();
        }
        let limit = max_width.saturating_sub(1).max(1);
        let mut current = 0;
        let mut trimmed = String::new();
        for ch in text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current + width > limit {
                break;
            }
            trimmed.push(ch);
            current += width;
        }
        if trimmed.is_empty() {
            "…".to_string()
        } else {
            trimmed.push('…');
            trimmed
        }
    }

    fn comment_target_for_context(&self) -> Result<CommentTarget> {
        if self.banner_selected() {
            bail!("Select a post before commenting.");
        }
        let post = self
            .posts
            .get(self.selected_post)
            .ok_or_else(|| anyhow!("Select a post before commenting."))?;
        if post.post.name.trim().is_empty() {
            bail!("This post cannot be commented on.");
        }

        if self.focused_pane != Pane::Comments {
            return Ok(CommentTarget::Post {
                post_fullname: post.post.name.clone(),
                post_title: post.post.title.clone(),
                subreddit: post.post.subreddit.clone(),
            });
        }

        if let Some(index) = self.selected_comment_index() {
            if let Some(comment) = self.comments.get(index) {
                if !comment.name.trim().is_empty() {
                    return Ok(CommentTarget::Comment {
                        post_fullname: post.post.name.clone(),
                        post_title: post.post.title.clone(),
                        comment_fullname: comment.name.clone(),
                        author: comment.author.clone(),
                    });
                }
            }
        }

        Ok(CommentTarget::Post {
            post_fullname: post.post.name.clone(),
            post_title: post.post.title.clone(),
            subreddit: post.post.subreddit.clone(),
        })
    }

    fn comment_action_label(target: &CommentTarget) -> String {
        match target {
            CommentTarget::Post { post_title, .. } => {
                let short = Self::ellipsize_label(post_title, 48);
                format!("Comment on \"{}\"", short)
            }
            CommentTarget::Comment { author, .. } => {
                if author.trim().is_empty() {
                    "Reply to comment".to_string()
                } else {
                    format!("Reply to u/{}", author.trim())
                }
            }
        }
    }

    fn open_comment_composer(&mut self) -> Result<()> {
        if self.comment_composer.is_some() {
            return Ok(());
        }
        if self.interaction_service.is_none() {
            self.status_message =
                "Sign in to a Reddit account before writing a comment.".to_string();
            self.mark_dirty();
            return Ok(());
        }
        if self.pending_comment_submit.is_some() {
            self.status_message = "A comment submission is already in progress.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let target = match self.comment_target_for_context() {
            Ok(result) => result,
            Err(err) => {
                self.status_message = err.to_string();
                self.mark_dirty();
                return Ok(());
            }
        };

        self.queue_active_kitty_delete();
        self.close_action_menu(None);
        self.help_visible = false;
        self.menu_visible = false;

        let prompt = match &target {
            CommentTarget::Post { post_title, .. } => {
                let short = Self::ellipsize_label(post_title, 48);
                format!("Commenting on \"{}\" — Ctrl+S submits, Esc cancels.", short)
            }
            CommentTarget::Comment { author, .. } => {
                let base = if author.trim().is_empty() {
                    "Replying to a deleted comment".to_string()
                } else {
                    format!("Replying to u/{}", author.trim())
                };
                format!("{base} — Ctrl+S submits, Esc cancels.")
            }
        };

        self.comment_composer = Some(CommentComposer::new(target));
        self.status_message = prompt;
        self.mark_dirty();
        Ok(())
    }

    fn insert_post_root_comment_placeholder(&mut self) {
        let message = if self.comments.iter().any(|entry| !entry.is_post_root) {
            "Comment section · w starts a new thread · w on a comment replies".to_string()
        } else {
            "Comment section · w starts the discussion".to_string()
        };

        if let Some(existing) = self.comments.iter_mut().find(|entry| entry.is_post_root) {
            existing.raw_body = message.clone();
            existing.body = message;
            return;
        }

        let placeholder = CommentEntry {
            name: String::new(),
            author: String::new(),
            raw_body: message.clone(),
            body: message,
            score: 0,
            likes: None,
            score_hidden: false,
            depth: 0,
            descendant_count: 0,
            links: Vec::new(),
            is_post_root: true,
        };

        self.comments.insert(0, placeholder);
    }

    fn insert_posted_comment(&mut self, comment: reddit::Comment, target: CommentTarget) {
        let post_name = target.post_fullname().to_string();
        if self
            .posts
            .get(self.selected_post)
            .map(|post| post.post.name.as_str())
            != Some(post_name.as_str())
        {
            return;
        }

        self.insert_post_root_comment_placeholder();

        let reddit::Comment {
            name,
            body,
            author,
            score,
            likes,
            score_hidden,
            depth,
            ..
        } = comment;

        let raw_body = body.clone();
        let (clean_body, found_links) = scrub_links(&body);
        let author_label = if author.trim().is_empty() {
            "[deleted]".to_string()
        } else {
            format!("u/{}", author.trim())
        };
        let mut link_entries = Vec::new();
        for (idx, url) in found_links.into_iter().enumerate() {
            let label = format!("Comment link {} ({author_label})", idx + 1);
            link_entries.push(LinkEntry::new(label, url));
        }

        let mut entry = CommentEntry {
            name,
            author,
            raw_body,
            body: clean_body,
            score,
            likes,
            score_hidden,
            depth: 0,
            descendant_count: 0,
            links: link_entries,
            is_post_root: false,
        };

        let mut insert_index = self.comments.len();

        match &target {
            CommentTarget::Post { .. } => {
                entry.depth = 0;
                if let Some(idx) = self
                    .comments
                    .iter()
                    .position(|comment| comment.is_post_root)
                {
                    insert_index = idx + 1;
                } else {
                    insert_index = 0;
                }
            }
            CommentTarget::Comment {
                comment_fullname, ..
            } => {
                if let Some(parent_idx) = self
                    .comments
                    .iter()
                    .position(|entry| entry.name == *comment_fullname)
                {
                    let parent_depth = self.comments[parent_idx].depth;
                    entry.depth = parent_depth + 1;
                    self.collapsed_comments.remove(&parent_idx);
                    insert_index = parent_idx + 1;
                    while insert_index < self.comments.len()
                        && self.comments[insert_index].depth > parent_depth
                    {
                        insert_index += 1;
                    }
                    let mut search_depth = entry.depth;
                    for idx in (0..=parent_idx).rev() {
                        if self.comments[idx].is_post_root {
                            continue;
                        }
                        if self.comments[idx].depth < search_depth {
                            self.comments[idx].descendant_count =
                                self.comments[idx].descendant_count.saturating_add(1);
                            search_depth = self.comments[idx].depth;
                            if search_depth == 0 {
                                break;
                            }
                        }
                    }
                } else {
                    entry.depth = depth.max(0) as usize;
                }
            }
        }

        if insert_index > self.comments.len() {
            insert_index = self.comments.len();
        }

        self.comments.insert(insert_index, entry);
        self.insert_post_root_comment_placeholder();

        if let Some(post) = self.posts.get_mut(self.selected_post) {
            if post.post.name == post_name {
                post.post.num_comments = post.post.num_comments.saturating_add(1);
                self.post_rows.remove(&post.post.name);
            }
        }

        self.rebuild_visible_comments_internal(Some(insert_index), false);
        self.recompute_comment_status();
        self.ensure_comment_visible();
        self.mark_dirty();
    }

    fn cancel_comment_composer(&mut self, message: Option<&str>) {
        if self.comment_composer.is_none() {
            return;
        }
        if self.pending_comment_submit.is_some() {
            if let Some(composer) = self.comment_composer.as_mut() {
                composer.set_status("Comment submission already in flight…");
            }
            self.status_message =
                "Comment submission already in progress; wait for it to finish.".to_string();
            self.mark_dirty();
            return;
        }
        self.comment_composer = None;
        if let Some(msg) = message {
            self.status_message = msg.to_string();
        } else {
            self.status_message = "Comment discarded.".to_string();
        }
        self.mark_dirty();
    }

    fn submit_comment(&mut self) -> Result<()> {
        let Some(composer) = self.comment_composer.as_mut() else {
            return Ok(());
        };
        if composer.submitting {
            return Ok(());
        }

        let service = match self.interaction_service.as_ref() {
            Some(service) => Arc::clone(service),
            None => {
                self.status_message =
                    "Sign in to a Reddit account before writing a comment.".to_string();
                self.mark_dirty();
                return Ok(());
            }
        };

        let text = composer.buffer.as_text();
        if text.trim().is_empty() {
            composer.set_status("Write something before submitting.");
            self.status_message = "Comment text cannot be empty.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let target = composer.target.clone();
        let parent = target.parent_fullname().to_string();
        let post_fullname = target.post_fullname().to_string();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);

        composer.submitting = true;
        composer.clear_status();
        self.status_message = "Posting comment…".to_string();
        self.pending_comment_submit = Some(PendingCommentSubmit {
            request_id,
            post_fullname,
            target: target.clone(),
        });
        self.mark_dirty();

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let payload = text;
            let result = service.reply(parent.as_str(), payload.as_str());
            let _ = tx.send(AsyncResponse::CommentSubmit { request_id, result });
        });

        Ok(())
    }

    fn handle_comment_composer_key(&mut self, key: KeyEvent) -> Result<bool> {
        let Some(composer_ref) = self.comment_composer.as_ref() else {
            return Ok(false);
        };
        if composer_ref.submitting {
            if matches!(key.code, KeyCode::Esc) {
                self.status_message =
                    "Comment submission already in progress; please wait.".to_string();
                self.mark_dirty();
            }
            return Ok(false);
        }

        let mut dirty = false;
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Esc => {
                self.cancel_comment_composer(Some("Comment discarded."));
                return Ok(false);
            }
            KeyCode::Enter
                if modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.submit_comment()?;
                return Ok(false);
            }
            KeyCode::Enter => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.insert_newline();
                    composer.clear_status();
                }
                dirty = true;
            }
            KeyCode::Backspace => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.backspace();
                    composer.clear_status();
                }
                dirty = true;
            }
            KeyCode::Delete => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.delete();
                    composer.clear_status();
                }
                dirty = true;
            }
            KeyCode::Left => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.move_left();
                }
                dirty = true;
            }
            KeyCode::Right => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.move_right();
                }
                dirty = true;
            }
            KeyCode::Up => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.move_up();
                }
                dirty = true;
            }
            KeyCode::Down => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.move_down();
                }
                dirty = true;
            }
            KeyCode::Home => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.move_home();
                }
                dirty = true;
            }
            KeyCode::End => {
                if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.move_end();
                }
                dirty = true;
            }
            KeyCode::Char(ch) => {
                if modifiers.contains(KeyModifiers::CONTROL)
                    || modifiers.contains(KeyModifiers::ALT)
                {
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(ch, 'm' | 'M' | 's' | 'S' | '\r' | '\n')
                    {
                        self.submit_comment()?;
                        return Ok(false);
                    }
                    if modifiers.contains(KeyModifiers::CONTROL) && matches!(ch, 'u' | 'U') {
                        if let Some(composer) = self.comment_composer.as_mut() {
                            while composer.buffer.cursor_col > 0 {
                                composer.buffer.backspace();
                            }
                            composer.clear_status();
                        }
                        dirty = true;
                    }
                } else if let Some(composer) = self.comment_composer.as_mut() {
                    composer.buffer.insert_char(ch);
                    composer.clear_status();
                    dirty = true;
                }
            }
            _ => {}
        }

        if dirty {
            self.mark_dirty();
        }
        Ok(false)
    }

    fn draw_comment_composer(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let Some(composer) = self.comment_composer.as_mut() else {
            return;
        };

        let popup = centered_rect(70, 70, area);
        frame.render_widget(Clear, popup);

        let title = Span::styled(
            "Write a comment",
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        );

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(COLOR_ACCENT))
            .style(Style::default().bg(COLOR_PANEL_BG));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(inner);

        let title_line = Self::ellipsize_label(composer.target.post_title(), 64);
        let header_lines: Vec<Line<'static>> = vec![
            Line::from(vec![Span::styled(
                title_line,
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![Span::styled(
                composer.target.description(),
                Style::default().fg(COLOR_TEXT_SECONDARY),
            )]),
        ];
        let header = Paragraph::new(Text::from(header_lines))
            .style(Style::default().fg(COLOR_TEXT_PRIMARY).bg(COLOR_PANEL_BG));
        frame.render_widget(header, sections[0]);

        let text_block = Block::default()
            .padding(Padding::new(1, 1, 0, 0))
            .style(Style::default().bg(COLOR_PANEL_BG));
        let text_inner = text_block.inner(sections[1]);

        let visible_height = text_inner.height.max(1) as usize;
        let max_scroll = composer.buffer.lines.len().saturating_sub(visible_height);
        if composer.scroll_row > max_scroll {
            composer.scroll_row = max_scroll;
        }
        if composer.buffer.cursor_row < composer.scroll_row {
            composer.scroll_row = composer.buffer.cursor_row;
        } else if composer.buffer.cursor_row >= composer.scroll_row.saturating_add(visible_height) {
            composer.scroll_row = composer
                .buffer
                .cursor_row
                .saturating_add(1)
                .saturating_sub(visible_height);
        }

        let text = Text::from(composer.buffer.as_text());
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(COLOR_TEXT_PRIMARY).bg(COLOR_PANEL_BG))
            .block(text_block)
            .scroll((composer.scroll_row as u16, 0));
        frame.render_widget(paragraph, sections[1]);

        let mut footer_lines: Vec<Line<'static>> = Vec::new();
        if composer.submitting {
            footer_lines.push(Line::from(vec![Span::styled(
                "Posting comment…",
                Style::default().fg(COLOR_ACCENT),
            )]));
        } else {
            footer_lines.push(Line::from(vec![Span::styled(
                "Ctrl+S submit · Esc cancel · Enter newline",
                Style::default().fg(COLOR_TEXT_SECONDARY),
            )]));
        }
        if let Some(status) = composer.status() {
            footer_lines.push(Line::from(vec![Span::styled(
                status.to_string(),
                Style::default().fg(COLOR_ERROR),
            )]));
        }
        let footer = Paragraph::new(Text::from(footer_lines))
            .style(Style::default().fg(COLOR_TEXT_PRIMARY).bg(COLOR_PANEL_BG));
        frame.render_widget(footer, sections[2]);

        if !composer.submitting {
            let mut cursor_x = text_inner.x;
            if let Some(line) = composer.buffer.lines.get(composer.buffer.cursor_row) {
                let visual_col = line
                    .chars()
                    .take(composer.buffer.cursor_col)
                    .fold(0u16, |acc, ch| {
                        acc + UnicodeWidthChar::width(ch).unwrap_or(0) as u16
                    });
                cursor_x = cursor_x.saturating_add(visual_col);
            }
            let max_x = text_inner
                .x
                .saturating_add(text_inner.width.saturating_sub(1));
            if cursor_x > max_x {
                cursor_x = max_x;
            }
            let cursor_y = text_inner
                .y
                .saturating_add(
                    (composer
                        .buffer
                        .cursor_row
                        .saturating_sub(composer.scroll_row)) as u16,
                )
                .min(
                    text_inner
                        .y
                        .saturating_add(text_inner.height.saturating_sub(1)),
                );
            frame.set_cursor(cursor_x, cursor_y);
        }
    }

    fn selected_comment_index(&self) -> Option<usize> {
        self.visible_comment_indices
            .get(self.selected_comment)
            .copied()
    }

    fn rebuild_visible_comments_internal(
        &mut self,
        preferred: Option<usize>,
        fallback_to_first: bool,
    ) {
        self.visible_comment_indices.clear();
        let mut new_selection = None;
        let mut hidden_depths: Vec<usize> = Vec::new();

        for (index, comment) in self.comments.iter().enumerate() {
            while hidden_depths
                .last()
                .is_some_and(|depth| *depth >= comment.depth)
            {
                hidden_depths.pop();
            }

            if !hidden_depths.is_empty() {
                continue;
            }

            let visible_index = self.visible_comment_indices.len();
            self.visible_comment_indices.push(index);

            if self.collapsed_comments.contains(&index) {
                hidden_depths.push(comment.depth);
            }

            if preferred == Some(index) {
                new_selection = Some(visible_index);
            }
        }

        if let Some(selection) = new_selection {
            self.selected_comment = selection;
        } else if self.visible_comment_indices.is_empty() || fallback_to_first {
            self.selected_comment = 0;
        } else {
            self.selected_comment = self
                .selected_comment
                .min(self.visible_comment_indices.len() - 1);
        }

        self.ensure_comment_visible();
    }

    fn rebuild_visible_comments_reset(&mut self) {
        self.rebuild_visible_comments_internal(None, true);
    }

    fn recompute_comment_status(&mut self) {
        let sort_display = comment_sort_label(self.comment_sort);

        let total_real = self
            .comments
            .iter()
            .filter(|entry| !entry.is_post_root)
            .count();

        if total_real == 0 {
            self.comment_status = format!("No comments yet · sorted by {}", sort_display);
            return;
        }

        let visible_real = self
            .visible_comment_indices
            .iter()
            .filter(|idx| {
                self.comments
                    .get(**idx)
                    .is_some_and(|entry| !entry.is_post_root)
            })
            .count();

        if visible_real == total_real {
            self.comment_status =
                format!("{total_real} comments loaded · sorted by {}", sort_display);
        } else {
            let hidden = total_real.saturating_sub(visible_real);
            self.comment_status = format!(
                "{total_real} comments loaded · {visible_real} visible · {hidden} hidden · sorted by {}",
                sort_display
            );
        }
    }

    fn toggle_selected_comment_fold(&mut self) {
        let Some(comment_index) = self.selected_comment_index() else {
            self.status_message = "No comment selected to fold.".to_string();
            return;
        };

        let entry = match self.comments.get(comment_index) {
            Some(entry) => entry,
            None => {
                self.status_message = "Comment selection is out of sync.".to_string();
                return;
            }
        };

        if entry.is_post_root {
            self.status_message =
                "Highlight the placeholder and press Enter or w to comment on the post."
                    .to_string();
            return;
        }

        if entry.descendant_count == 0 {
            self.status_message = "Comment has no replies to fold.".to_string();
            return;
        }

        if self.collapsed_comments.remove(&comment_index) {
            self.status_message = "Expanded comment thread.".to_string();
        } else {
            self.collapsed_comments.insert(comment_index);
            let replies = entry.descendant_count;
            let suffix = if replies == 1 { "reply" } else { "replies" };
            self.status_message = format!("Collapsed {replies} {suffix}.");
        }

        self.rebuild_visible_comments_internal(Some(comment_index), false);
        self.recompute_comment_status();
        self.mark_dirty();
    }

    fn expand_all_comments(&mut self) {
        if self.collapsed_comments.is_empty() {
            self.status_message = "All comments already expanded.".to_string();
            return;
        }

        let preferred = self.selected_comment_index();
        self.collapsed_comments.clear();
        self.rebuild_visible_comments_internal(preferred, false);
        self.recompute_comment_status();
        self.status_message = "Expanded all comment threads.".to_string();
        self.mark_dirty();
    }

    fn select_post_at(&mut self, index: usize) {
        if self.posts.is_empty() {
            self.selected_post = 0;
            self.post_offset.set(0);
            self.update_banner_selected = false;
            return;
        }

        let max_index = self.posts.len() - 1;
        let clamped = index.min(max_index);
        let changed = clamped != self.selected_post;

        self.update_banner_selected = false;
        self.selected_post = clamped;
        if changed {
            self.dismiss_release_note();
            self.queue_active_kitty_delete();
            self.comment_offset.set(0);
            self.close_action_menu(None);
            self.comment_sort_selected = false;
            let _ = self.stop_active_video(None, true);
            self.video_completed_post = None;
            self.sync_content_from_selection();
            if let Err(err) = self.load_comments_for_selection() {
                self.comment_status = format!("Failed to load comments: {err}");
            }
        }

        self.ensure_post_visible();
        self.maybe_request_more_posts();
    }

    fn maybe_request_more_posts(&mut self) {
        if self.posts.is_empty() {
            return;
        }
        if self.pending_posts.is_some() {
            return;
        }
        let Some(after) = self.feed_after.as_ref() else {
            return;
        };
        if after.trim().is_empty() {
            return;
        }
        let remaining = self
            .posts
            .len()
            .saturating_sub(self.selected_post.saturating_add(1));
        if remaining > POST_PRELOAD_THRESHOLD {
            return;
        }
        if let Err(err) = self.load_more_posts() {
            self.status_message = format!("Failed to load more posts: {err}");
        }
    }

    fn listing_over18_params(&self) -> Vec<(String, String)> {
        let flag = if self.show_nsfw { "on" } else { "off" };
        vec![
            ("include_over_18".to_string(), flag.to_string()),
            ("search_include_over_18".to_string(), flag.to_string()),
        ]
    }

    fn filter_nsfw_posts(&self, posts: &mut Vec<PostPreview>) -> usize {
        if self.show_nsfw {
            return 0;
        }
        let original_len = posts.len();
        posts.retain(|preview| !preview.post.over_18);
        original_len.saturating_sub(posts.len())
    }

    fn toggle_nsfw_filter(&mut self) -> Result<()> {
        self.show_nsfw = !self.show_nsfw;
        let toggle_message = if self.show_nsfw {
            "NSFW posts enabled — refreshing feed..."
        } else {
            "NSFW posts hidden — refreshing feed..."
        }
        .to_string();

        let persist = self.store.set_show_nsfw_posts(self.show_nsfw);
        match &persist {
            Ok(()) => {
                self.status_message = toggle_message.clone();
            }
            Err(err) => {
                self.status_message = format!("Failed to save NSFW preference: {err}");
            }
        }
        self.mark_dirty();

        if let Err(err) = self.reload_posts() {
            self.status_message = format!("Failed to reload after NSFW toggle: {err}");
            self.mark_dirty();
            return Err(err);
        }

        if persist.is_ok() {
            self.status_message = toggle_message;
            self.mark_dirty();
        }

        Ok(())
    }

    fn current_feed_target(&self) -> String {
        if self.subreddits.is_empty() {
            return "r/frontpage".to_string();
        }
        let index = self
            .selected_sub
            .min(self.subreddits.len().saturating_sub(1));
        self.subreddits
            .get(index)
            .cloned()
            .unwrap_or_else(|| "r/frontpage".to_string())
    }

    fn select_subreddit_by_name(&mut self, name: &str) -> bool {
        if self.subreddits.is_empty() {
            return false;
        }
        if let Some(idx) = self
            .subreddits
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            self.selected_sub = idx;
            let nav_base = NAV_SORTS.len();
            let nav_len = nav_base.saturating_add(self.subreddits.len());
            let desired = nav_base.saturating_add(idx);
            if nav_len > 0 {
                self.nav_index = desired.min(nav_len.saturating_sub(1));
            } else {
                self.nav_index = 0;
            }
            self.nav_mode = NavMode::Subreddits;
            self.ensure_subreddit_visible();
            true
        } else {
            false
        }
    }

    fn ensure_subreddit_visible(&self) {
        let len = self.subreddits.len();
        if len == 0 {
            self.subreddit_offset.set(0);
            return;
        }

        let target = self.nav_index.min(len - 1);
        let viewport_height = self.subreddit_view_height.get() as usize;
        if viewport_height == 0 {
            self.subreddit_offset.set(target);
            return;
        }

        let width = self.subreddit_view_width.get().max(1) as usize;
        let mut prefix = vec![0usize; target.saturating_add(1) + 1];
        for idx in 0..=target {
            let height = self.subreddit_item_height(idx, width);
            prefix[idx + 1] = prefix[idx].saturating_add(height);
        }

        let selection_height = self.subreddit_item_height(target, width);
        let total_to_selection = prefix[target + 1];
        let lower_bound = (viewport_height as f32) * 0.25;
        let upper_bound = (viewport_height as f32) * 0.75;
        let midpoint = (viewport_height as f32) * 0.5;

        let mut best_choice: Option<(usize, f32)> = None;
        let mut fallback_choice: Option<(usize, f32)> = None;

        for (candidate, start) in prefix.iter().take(target + 1).enumerate() {
            let bottom = total_to_selection.saturating_sub(*start);
            if bottom > viewport_height {
                continue;
            }
            let top = bottom.saturating_sub(selection_height);
            let center = top as f32 + (selection_height as f32 / 2.0);
            let diff = (center - midpoint).abs();

            if center >= lower_bound && center <= upper_bound {
                match best_choice {
                    Some((_, best_diff)) if diff >= best_diff => {}
                    _ => best_choice = Some((candidate, diff)),
                }
            }

            match fallback_choice {
                Some((_, best_diff)) if diff >= best_diff => {}
                _ => fallback_choice = Some((candidate, diff)),
            }
        }

        let chosen = best_choice
            .or(fallback_choice)
            .map(|(candidate, _)| candidate)
            .unwrap_or(target);

        self.subreddit_offset.set(chosen);
    }

    fn subreddit_item_height(&self, index: usize, width: usize) -> usize {
        let Some(name) = self.subreddits.get(index) else {
            return 1;
        };

        let style = Style::default();
        let display = navigation_display_name(name);
        let mut height = wrap_plain(&display, width, style).len().saturating_add(1);
        if height == 0 {
            height = 1;
        }
        height
    }

    fn ensure_post_visible(&self) {
        let len = self.posts.len();
        if len == 0 {
            self.post_offset.set(0);
            return;
        }

        if self.post_view_height.get() == 0 {
            self.post_offset.set(self.selected_post.min(len - 1));
            return;
        }

        let selected = self.selected_post.min(len - 1);
        let mut height_cache: Vec<Option<usize>> = vec![None; selected + 1];
        let mut height_for = |idx: usize| -> usize {
            if idx > selected {
                return 0;
            }
            if let Some(height) = height_cache[idx] {
                return height;
            }
            let height = self.post_item_height(idx).max(1);
            height_cache[idx] = Some(height);
            height
        };

        let mut prefix = vec![0usize; selected.saturating_add(1) + 1];
        for idx in 0..=selected {
            prefix[idx + 1] = prefix[idx].saturating_add(height_for(idx));
        }

        let span_height = |start: usize, end: usize| -> usize {
            if start > end {
                return 0;
            }
            prefix[end + 1].saturating_sub(prefix[start])
        };

        let mut best_choice: Option<(usize, f32)> = None;
        let mut fallback_choice: Option<(usize, f32)> = None;

        for candidate in 0..=selected {
            let available = self.available_post_height(candidate);
            if available == 0 {
                continue;
            }

            let selection_height = height_for(selected);
            let bottom = span_height(candidate, selected);
            if bottom > available {
                continue;
            }

            let top = bottom.saturating_sub(selection_height);
            let center = top as f32 + (selection_height as f32 / 2.0);
            let lower_bound = (available as f32) * 0.25;
            let upper_bound = (available as f32) * 0.75;
            let midpoint = (available as f32) * 0.5;
            let diff = (center - midpoint).abs();

            if center >= lower_bound && center <= upper_bound {
                match best_choice {
                    Some((_, best_diff)) if diff >= best_diff => {}
                    _ => best_choice = Some((candidate, diff)),
                }
            }

            match fallback_choice {
                Some((_, best_diff)) if diff >= best_diff => {}
                _ => fallback_choice = Some((candidate, diff)),
            }
        }

        let chosen = best_choice
            .or(fallback_choice)
            .map(|(candidate, _)| candidate)
            .unwrap_or(selected);

        self.post_offset.set(chosen);
    }

    fn ensure_comment_visible(&self) {
        let len = self.visible_comment_indices.len();
        if len == 0 {
            self.comment_offset.set(0);
            return;
        }

        if self.comment_view_height.get() <= 1 {
            self.comment_offset.set(0);
            return;
        }

        let available = self.available_comment_height();
        if available == 0 {
            self.comment_offset.set(0);
            return;
        }

        let selected = self.selected_comment.min(len - 1);
        let mut height_cache: Vec<Option<usize>> = vec![None; selected.saturating_add(1)];
        let mut height_for = |idx: usize| -> usize {
            if idx > selected {
                return 0;
            }
            if let Some(height) = height_cache.get(idx).and_then(|cached| *cached) {
                return height;
            }
            let height = self.comment_item_height(idx).max(1);
            if let Some(slot) = height_cache.get_mut(idx) {
                *slot = Some(height);
            }
            height
        };

        let mut prefix = vec![0usize; selected.saturating_add(1) + 1];
        for idx in 0..=selected {
            prefix[idx + 1] = prefix[idx].saturating_add(height_for(idx));
        }

        let span_height = |start: usize, end: usize| -> usize {
            if start > end {
                return 0;
            }
            prefix[end + 1].saturating_sub(prefix[start])
        };

        let mut best_choice: Option<(usize, f32)> = None;
        let mut fallback_choice: Option<(usize, f32)> = None;
        let selection_height = height_for(selected);
        let lower_bound = (available as f32) * 0.25;
        let upper_bound = (available as f32) * 0.75;
        let midpoint = (available as f32) * 0.5;

        for candidate in 0..=selected {
            let bottom = span_height(candidate, selected);
            if bottom > available {
                continue;
            }
            let top = bottom.saturating_sub(selection_height);
            let center = top as f32 + (selection_height as f32 / 2.0);
            let diff = (center - midpoint).abs();

            if center >= lower_bound && center <= upper_bound {
                match best_choice {
                    Some((_, best_diff)) if diff >= best_diff => {}
                    _ => best_choice = Some((candidate, diff)),
                }
            }

            match fallback_choice {
                Some((_, best_diff)) if diff >= best_diff => {}
                _ => fallback_choice = Some((candidate, diff)),
            }
        }

        let chosen = best_choice
            .or(fallback_choice)
            .map(|(candidate, _)| candidate)
            .unwrap_or(selected);

        self.comment_offset.set(chosen);
    }

    fn post_item_height(&self, index: usize) -> usize {
        let Some(post) = self.posts.get(index) else {
            return 0;
        };
        if let Some(row) = self.post_rows.get(&post.post.name) {
            row.identity
                .len()
                .saturating_add(row.title.len())
                .saturating_add(row.metrics.len())
                .saturating_add(1)
        } else {
            3
        }
    }

    fn available_post_height(&self, offset: usize) -> usize {
        let mut base = self.post_view_height.get() as usize;
        if base == 0 {
            return 0;
        }
        if self.update_notice.is_some() {
            base = base.saturating_sub(UPDATE_BANNER_HEIGHT);
        }
        if offset == 0 && self.pending_posts.is_some() && !self.posts.is_empty() {
            base = base.saturating_sub(POST_LOADING_HEADER_HEIGHT);
        }
        base
    }

    fn comment_item_height(&self, visible_index: usize) -> usize {
        let Some(&comment_index) = self.visible_comment_indices.get(visible_index) else {
            return 0;
        };
        let Some(comment) = self.comments.get(comment_index) else {
            return 0;
        };
        let width = self.comment_view_width.get().max(1) as usize;
        let collapsed = self.collapsed_comments.contains(&comment_index);
        let indicator = if collapsed { "[+]" } else { "[-]" };
        let meta_style = Style::default();
        let body_style = Style::default();
        let lines = comment_lines(comment, width, indicator, meta_style, body_style, collapsed);
        lines.len().saturating_add(1)
    }

    fn available_comment_height(&self) -> usize {
        let total = self.comment_view_height.get() as usize;
        if total == 0 {
            return 0;
        }
        let status_height = self.comment_status_height.get().min(total);
        total.saturating_sub(status_height)
    }

    fn posts_page_step(&self) -> i32 {
        let visible = self.post_view_height.get();
        let visible = if visible == 0 { 1 } else { visible as usize };
        let step = visible.saturating_sub(1).max(1);
        step as i32
    }

    fn set_sort_by_index(&mut self, index: usize) -> Result<()> {
        if index >= NAV_SORTS.len() {
            return Ok(());
        }
        let sort = NAV_SORTS[index];
        if self.sort != sort {
            self.sort = sort;
            self.reload_posts()?;
        }
        self.nav_mode = NavMode::Sorts;
        self.status_message = format!("Sort set to {}", sort_label(sort));
        Ok(())
    }

    fn shift_sort(&mut self, delta: i32) -> Result<()> {
        let len = NAV_SORTS.len() as i32;
        if len == 0 {
            return Ok(());
        }
        let current = NAV_SORTS
            .iter()
            .position(|candidate| *candidate == self.sort)
            .unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.set_sort_by_index(next as usize)
    }

    fn comment_sort_index(&self) -> usize {
        COMMENT_SORTS
            .iter()
            .position(|candidate| *candidate == self.comment_sort)
            .unwrap_or(0)
    }

    fn set_comment_sort_by_index(&mut self, index: usize) -> Result<()> {
        if index >= COMMENT_SORTS.len() {
            return Ok(());
        }
        let sort = COMMENT_SORTS[index];
        if self.comment_sort != sort {
            self.comment_sort = sort;
            self.status_message = format!("Comments sorted by {}", comment_sort_label(sort));
            self.load_comments_for_selection()?;
            self.comment_sort_selected = true;
        } else {
            self.status_message =
                format!("Comments already sorted by {}", comment_sort_label(sort));
        }
        self.mark_dirty();
        Ok(())
    }

    fn shift_comment_sort(&mut self, delta: i32) -> Result<()> {
        let len = COMMENT_SORTS.len() as i32;
        if len == 0 {
            return Ok(());
        }
        let current = self.comment_sort_index() as i32;
        let next = (current + delta).rem_euclid(len);
        self.set_comment_sort_by_index(next as usize)
    }

    fn cache_posts(&mut self, key: FeedCacheKey, batch: PostBatch) {
        if self.feed_cache.len() >= FEED_CACHE_MAX {
            if let Some(old_key) = self
                .feed_cache
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(key, _)| key.clone())
            {
                self.feed_cache.remove(&old_key);
            }
        }
        self.feed_cache.insert(
            key,
            FeedCacheEntry {
                batch,
                fetched_at: Instant::now(),
                scope: self.cache_scope,
            },
        );
    }

    fn cache_comments(
        &mut self,
        post_name: &str,
        sort: reddit::CommentSortOption,
        comments: Vec<CommentEntry>,
    ) {
        if self.comment_cache.len() >= COMMENT_CACHE_MAX {
            if let Some(old_key) = self
                .comment_cache
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(key, _)| key.clone())
            {
                self.comment_cache.remove(&old_key);
            }
        }
        let key = CommentCacheKey::new(post_name, sort);
        self.comment_cache.insert(
            key,
            CommentCacheEntry {
                comments,
                fetched_at: Instant::now(),
                scope: self.cache_scope,
            },
        );
    }

    fn apply_posts_batch(
        &mut self,
        target: &str,
        sort: reddit::SortOption,
        mut batch: PostBatch,
        from_cache: bool,
        mode: LoadMode,
    ) {
        let filtered_nsfw = self.filter_nsfw_posts(&mut batch.posts);
        let label = navigation_display_name(target);
        match mode {
            LoadMode::Replace => {
                if batch.posts.is_empty() {
                    let mut handled = false;
                    if filtered_nsfw > 0 && !self.show_nsfw {
                        self.status_message = format!(
                            "All posts hidden by NSFW filter for {} ({}). Press n to show NSFW posts.",
                            label,
                            sort_label(sort)
                        );
                        handled = true;
                    } else if !from_cache {
                        if let Some(fallback) = fallback_feed_target(target) {
                            if self.select_subreddit_by_name(fallback) {
                                self.feed_after = None;
                                let fallback_label = navigation_display_name(fallback);
                                self.status_message = format!(
                                    "No posts available for {} ({}) — loading {} instead...",
                                    label,
                                    sort_label(sort),
                                    fallback_label
                                );
                                if let Err(err) = self.reload_posts() {
                                    self.status_message = format!(
                                        "Tried {}, but failed to load posts: {}",
                                        fallback_label, err
                                    );
                                }
                                self.mark_dirty();
                                return;
                            }
                        }
                    }

                    if !handled {
                        let source = if from_cache { "(cached)" } else { "" };
                        self.status_message = format!(
                            "No posts available for {} ({}) {}",
                            label,
                            sort_label(sort),
                            source
                        )
                        .trim()
                        .to_string();
                    }
                    self.queue_active_kitty_delete();
                    self.posts.clear();
                    self.feed_after = batch.after.take();
                    self.post_offset.set(0);
                    self.numeric_jump = None;
                    self.content_scroll = 0;
                    self.content = self.fallback_content.clone();
                    self.content_source = self.fallback_source.clone();
                    self.comments.clear();
                    self.collapsed_comments.clear();
                    self.visible_comment_indices.clear();
                    self.comment_offset.set(0);
                    self.comment_status = "No comments available.".to_string();
                    self.selected_comment = 0;
                    self.close_action_menu(None);
                    self.content_cache.clear();
                    self.post_rows.clear();
                    self.post_rows_width = 0;
                    self.pending_post_rows = None;
                    self.media_previews.clear();
                    self.media_layouts.clear();
                    self.media_failures.clear();
                    self.needs_kitty_flush = false;
                    self.content_area = None;
                    if let Some(pending) = self.pending_content.take() {
                        pending.cancel_flag.store(true, Ordering::SeqCst);
                    }
                    return;
                }

                self.status_message = if from_cache {
                    format!(
                        "Loaded {} posts from {} ({}) — cached",
                        batch.posts.len(),
                        label,
                        sort_label(sort)
                    )
                } else {
                    format!(
                        "Loaded {} posts from {} ({})",
                        batch.posts.len(),
                        label,
                        sort_label(sort)
                    )
                };
                if filtered_nsfw > 0 && !self.show_nsfw {
                    self.status_message.push_str(&format!(
                        " · Hid {} NSFW post{}",
                        filtered_nsfw,
                        if filtered_nsfw == 1 { "" } else { "s" }
                    ));
                }
                self.queue_active_kitty_delete();
                self.posts = batch.posts;
                self.feed_after = batch.after;
                self.post_offset.set(0);
                self.comment_offset.set(0);
                self.close_action_menu(None);
                self.numeric_jump = None;
                self.media_previews
                    .retain(|key, _| self.posts.iter().any(|post| post.post.name == *key));
                self.media_layouts
                    .retain(|key, _| self.posts.iter().any(|post| post.post.name == *key));
                self.media_failures
                    .retain(|key| self.posts.iter().any(|post| post.post.name == *key));
                self.pending_media.retain(|key, flag| {
                    let keep = self.posts.iter().any(|post| post.post.name == *key);
                    if !keep {
                        flag.store(true, Ordering::SeqCst);
                    }
                    keep
                });
                self.pending_media_order
                    .retain(|key| self.pending_media.contains_key(key));
                self.comment_cache.retain(|key, _| {
                    self.posts
                        .iter()
                        .any(|post| post.post.name == key.post_name)
                });
                self.content_cache
                    .retain(|key, _| self.posts.iter().any(|post| post.post.name == *key));
                self.post_rows
                    .retain(|key, _| self.posts.iter().any(|post| post.post.name == *key));
                self.pending_post_rows = None;
                self.post_rows_width = 0;
                if let Some(pending) = self.pending_content.take() {
                    pending.cancel_flag.store(true, Ordering::SeqCst);
                }
                self.selected_post = 0;
                if !self.release_note_active {
                    self.sync_content_from_selection();
                }
                self.selected_comment = 0;
                self.comment_status = format!(
                    "Loading comments... · sorted by {}",
                    comment_sort_label(self.comment_sort)
                );
                self.comments.clear();
                self.collapsed_comments.clear();
                self.visible_comment_indices.clear();
                self.comment_offset.set(0);
                if let Err(err) = self.load_comments_for_selection() {
                    self.comment_status = format!("Failed to load comments: {err}");
                }
                self.ensure_post_visible();
            }
            LoadMode::Append => {
                let previous_after = self.feed_after.clone();
                self.feed_after = batch.after.clone();
                if batch.posts.is_empty() {
                    if filtered_nsfw > 0 && !self.show_nsfw {
                        self.status_message = format!(
                            "Hidden {} NSFW post{} from {} ({}) — requesting more...",
                            filtered_nsfw,
                            if filtered_nsfw == 1 { "" } else { "s" },
                            label,
                            sort_label(sort)
                        );
                        self.maybe_request_more_posts();
                        return;
                    }
                    if self.feed_after.is_none() || self.feed_after == previous_after {
                        if self.feed_after.is_none() {
                            self.status_message =
                                format!("Reached end of {} ({})", label, sort_label(sort));
                        } else {
                            self.status_message = format!(
                                "No additional posts returned for {} ({}).",
                                label,
                                sort_label(sort)
                            );
                        }
                    } else {
                        self.status_message = format!(
                            "No additional posts returned yet for {} ({}); requesting more...",
                            label,
                            sort_label(sort)
                        );
                        self.maybe_request_more_posts();
                    }
                    return;
                }

                let mut seen: HashSet<String> = self
                    .posts
                    .iter()
                    .map(|post| post.post.name.clone())
                    .collect();
                let original_incoming = batch.posts.len();
                batch
                    .posts
                    .retain(|post| seen.insert(post.post.name.clone()));

                if batch.posts.is_empty() {
                    if self.feed_after.is_none() || self.feed_after == previous_after {
                        if self.feed_after.is_none() {
                            self.status_message =
                                format!("Reached end of {} ({})", label, sort_label(sort));
                        } else {
                            self.status_message = format!(
                                "Skipped {} duplicate post{} from {} ({}).",
                                original_incoming,
                                if original_incoming == 1 { "" } else { "s" },
                                label,
                                sort_label(sort)
                            );
                        }
                    } else {
                        self.status_message = format!(
                            "Skipped {} duplicate post{} from {} ({}); requesting more...",
                            original_incoming,
                            if original_incoming == 1 { "" } else { "s" },
                            label,
                            sort_label(sort)
                        );
                        self.maybe_request_more_posts();
                    }
                    return;
                }

                let added = batch.posts.len();
                self.posts.extend(batch.posts);
                self.status_message = format!(
                    "Loaded {} more posts from {} ({}) — {} total.",
                    added,
                    label,
                    sort_label(sort),
                    self.posts.len()
                );
                if filtered_nsfw > 0 && !self.show_nsfw {
                    self.status_message.push_str(&format!(
                        " · Hid {} NSFW post{}",
                        filtered_nsfw,
                        if filtered_nsfw == 1 { "" } else { "s" }
                    ));
                }
                self.ensure_post_visible();
                self.maybe_request_more_posts();
            }
        }
    }

    fn is_loading(&self) -> bool {
        self.pending_posts.is_some()
            || self.pending_comments.is_some()
            || self.pending_content.is_some()
            || self.login_in_progress
            || !self.pending_media.is_empty()
            || self.media_save_in_progress.is_some()
    }

    fn reload_posts(&mut self) -> Result<()> {
        self.ensure_cache_scope();
        let Some(service) = &self.feed_service else {
            self.pending_posts = None;
            self.pending_comments = None;
            self.queue_active_kitty_delete();
            self.posts.clear();
            self.feed_after = None;
            self.post_offset.set(0);
            self.numeric_jump = None;
            self.content_scroll = 0;
            self.content = self.fallback_content.clone();
            self.content_source = self.fallback_source.clone();
            self.status_message = "Sign in to load Reddit posts.".to_string();
            self.comments.clear();
            self.collapsed_comments.clear();
            self.visible_comment_indices.clear();
            self.comment_offset.set(0);
            self.comment_status = "Sign in to load comments.".to_string();
            self.media_previews.clear();
            self.media_layouts.clear();
            self.media_failures.clear();
            for flag in self.pending_media.values() {
                flag.store(true, Ordering::SeqCst);
            }
            self.pending_media.clear();
            self.pending_media_order.clear();
            self.needs_kitty_flush = false;
            self.content_area = None;
            return Ok(());
        };

        let target = self.current_feed_target();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let sort = self.sort;
        let cache_key = FeedCacheKey::new(&target, sort);

        if let Some(entry) = self.feed_cache.get(&cache_key) {
            if entry.scope == self.cache_scope && entry.fetched_at.elapsed() < FEED_CACHE_TTL {
                if let Some(pending) = self.pending_posts.take() {
                    pending.cancel_flag.store(true, Ordering::SeqCst);
                }
                self.pending_posts = None;
                if let Some(pending) = self.pending_comments.take() {
                    pending.cancel_flag.store(true, Ordering::SeqCst);
                }
                self.apply_posts_batch(&target, sort, entry.batch.clone(), true, LoadMode::Replace);
                self.mark_dirty();
                return Ok(());
            }
        }

        if let Some(pending) = self.pending_posts.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        if let Some(pending) = self.pending_comments.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.pending_posts = Some(PendingPosts {
            request_id,
            cancel_flag: cancel_flag.clone(),
            mode: LoadMode::Replace,
        });
        self.feed_after = None;
        let feed_kind = classify_feed_target(&target);
        let label = navigation_display_name(&target);
        self.status_message = match feed_kind {
            FeedKind::Search(query) => {
                format!(
                    "Searching Reddit for \"{}\" ({})...",
                    query,
                    sort_label(sort)
                )
            }
            FeedKind::User(user) => {
                format!("Loading u/{} ({})...", user, sort_label(sort))
            }
            _ => format!("Loading {} ({})...", label, sort_label(sort)),
        };
        self.spinner.reset();

        let tx = self.response_tx.clone();
        let service = service.clone();
        let target_for_thread = target.clone();
        let opts = reddit::ListingOptions {
            after: None,
            extra: self.listing_over18_params(),
            ..Default::default()
        };

        thread::spawn(move || {
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let result =
                match classify_feed_target(&target_for_thread) {
                    FeedKind::FrontPage => {
                        service
                            .load_front_page(sort, opts.clone())
                            .map(|listing| PostBatch {
                                after: listing.after,
                                posts: listing
                                    .children
                                    .into_iter()
                                    .map(|thing| make_preview(thing.data))
                                    .collect::<Vec<_>>(),
                            })
                    }
                    FeedKind::Subreddit(name) => service
                        .load_subreddit(name, sort, opts.clone())
                        .map(|listing| PostBatch {
                            after: listing.after,
                            posts: listing
                                .children
                                .into_iter()
                                .map(|thing| make_preview(thing.data))
                                .collect::<Vec<_>>(),
                        }),
                    FeedKind::User(name) => {
                        service
                            .load_user(name, sort, opts.clone())
                            .map(|listing| PostBatch {
                                after: listing.after,
                                posts: listing
                                    .children
                                    .into_iter()
                                    .map(|thing| make_preview(thing.data))
                                    .collect::<Vec<_>>(),
                            })
                    }
                    FeedKind::Search(query) => {
                        service
                            .search_posts(query, sort, opts.clone())
                            .map(|listing| PostBatch {
                                after: listing.after,
                                posts: listing
                                    .children
                                    .into_iter()
                                    .map(|thing| make_preview(thing.data))
                                    .collect::<Vec<_>>(),
                            })
                    }
                };

            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }

            let _ = tx.send(AsyncResponse::Posts {
                request_id,
                target: target_for_thread,
                sort,
                result,
            });
        });
        Ok(())
    }

    fn load_more_posts(&mut self) -> Result<()> {
        self.ensure_cache_scope();
        if self.pending_posts.is_some() {
            return Ok(());
        }
        let Some(after) = self.feed_after.clone() else {
            return Ok(());
        };
        if after.trim().is_empty() {
            return Ok(());
        }
        let Some(service) = &self.feed_service else {
            return Ok(());
        };

        let target = self.current_feed_target();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let sort = self.sort;

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.pending_posts = Some(PendingPosts {
            request_id,
            cancel_flag: cancel_flag.clone(),
            mode: LoadMode::Append,
        });
        let feed_kind = classify_feed_target(&target);
        let label = navigation_display_name(&target);
        self.status_message = match feed_kind {
            FeedKind::Search(query) => format!(
                "Searching for additional posts matching \"{}\" ({})...",
                query,
                sort_label(sort)
            ),
            FeedKind::User(user) => format!(
                "Loading more posts from u/{} ({})...",
                user,
                sort_label(sort)
            ),
            _ => format!(
                "Loading more posts from {} ({})...",
                label,
                sort_label(sort)
            ),
        };
        self.spinner.reset();

        let tx = self.response_tx.clone();
        let service = service.clone();
        let target_for_thread = target.clone();
        let opts = reddit::ListingOptions {
            after: Some(after),
            extra: self.listing_over18_params(),
            ..Default::default()
        };

        thread::spawn(move || {
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let result =
                match classify_feed_target(&target_for_thread) {
                    FeedKind::FrontPage => {
                        service
                            .load_front_page(sort, opts.clone())
                            .map(|listing| PostBatch {
                                after: listing.after,
                                posts: listing
                                    .children
                                    .into_iter()
                                    .map(|thing| make_preview(thing.data))
                                    .collect::<Vec<_>>(),
                            })
                    }
                    FeedKind::Subreddit(name) => service
                        .load_subreddit(name, sort, opts.clone())
                        .map(|listing| PostBatch {
                            after: listing.after,
                            posts: listing
                                .children
                                .into_iter()
                                .map(|thing| make_preview(thing.data))
                                .collect::<Vec<_>>(),
                        }),
                    FeedKind::User(name) => {
                        service
                            .load_user(name, sort, opts.clone())
                            .map(|listing| PostBatch {
                                after: listing.after,
                                posts: listing
                                    .children
                                    .into_iter()
                                    .map(|thing| make_preview(thing.data))
                                    .collect::<Vec<_>>(),
                            })
                    }
                    FeedKind::Search(query) => {
                        service
                            .search_posts(query, sort, opts.clone())
                            .map(|listing| PostBatch {
                                after: listing.after,
                                posts: listing
                                    .children
                                    .into_iter()
                                    .map(|thing| make_preview(thing.data))
                                    .collect::<Vec<_>>(),
                            })
                    }
                };

            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }

            let _ = tx.send(AsyncResponse::Posts {
                request_id,
                target: target_for_thread,
                sort,
                result,
            });
        });

        Ok(())
    }

    fn reload_subreddits(&mut self) -> Result<()> {
        self.ensure_cache_scope();
        let Some(service) = &self.subreddit_service else {
            self.pending_subreddits = None;
            self.status_message = "Subreddit list unavailable without login.".to_string();
            return Ok(());
        };

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.pending_subreddits = Some(PendingSubreddits { request_id });
        self.status_message = "Refreshing subreddit list...".to_string();

        let tx = self.response_tx.clone();
        let service = service.clone();
        thread::spawn(move || {
            let result = service
                .list_subreddits(reddit::SubredditSource::Subscriptions)
                .map(|listing| listing.into_iter().map(|sub| sub.name).collect::<Vec<_>>());
            let _ = tx.send(AsyncResponse::Subreddits { request_id, result });
        });

        Ok(())
    }

    fn sync_content_from_selection(&mut self) {
        if self.release_note_active {
            return;
        }
        self.content_scroll = 0;
        self.needs_kitty_flush = false;
        if let Some(post) = self.posts.get(self.selected_post).cloned() {
            let key = post.post.name.clone();
            let source = content_from_post(&post);
            self.content_source = source.clone();

            if self
                .pending_content
                .as_ref()
                .map(|pending| pending.post_name != key)
                .unwrap_or(false)
            {
                if let Some(pending) = self.pending_content.take() {
                    pending.cancel_flag.store(true, Ordering::SeqCst);
                }
            }

            if let Some(cached) = self.content_cache.get(&key).cloned() {
                self.content = self.compose_content(cached, &post);
                self.ensure_media_request_ready(&post);
            } else {
                let placeholder = Text::from(vec![Line::from(Span::styled(
                    "Rendering content...",
                    Style::default().fg(COLOR_TEXT_SECONDARY),
                ))]);
                self.content = self.compose_content(placeholder, &post);
                self.queue_content_render(key.clone(), source);
            }

            if self.media_fullscreen {
                self.content = self.compose_fullscreen_preview(&post);
            }
        } else {
            self.content_source = self.fallback_source.clone();
            self.content = self.fallback_content.clone();
        }
    }

    fn toggle_media_fullscreen(&mut self) -> Result<()> {
        if self.media_fullscreen {
            let _ = self.stop_active_video(None, true);
            let previous_focus = self.media_fullscreen_prev_focus.take();
            let previous_scroll = self.media_fullscreen_prev_scroll.take();
            self.media_fullscreen = false;
            if let Some(focus) = previous_focus {
                self.focused_pane = focus;
            }
            self.queue_active_kitty_delete();
            self.sync_content_from_selection();
            if let Some(scroll) = previous_scroll {
                let max_scroll = self
                    .content
                    .lines
                    .len()
                    .saturating_sub(1)
                    .min(u16::MAX as usize) as u16;
                self.content_scroll = scroll.min(max_scroll);
            }
            self.pending_external_video = None;
            self.status_message = "Fullscreen preview closed.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let inline_video_source = self
            .active_video
            .as_ref()
            .map(|active| active.source.clone());
        let _ = self.stop_active_video(None, true);

        let Some(post) = self.posts.get(self.selected_post).cloned() else {
            self.status_message =
                "Select a post with media before toggling fullscreen.".to_string();
            self.mark_dirty();
            return Ok(());
        };

        let video_source = inline_video_source
            .or_else(|| {
                self.media_previews
                    .get(&post.post.name)
                    .and_then(|preview| preview.video().map(|video| video.source.clone()))
            })
            .or_else(|| video::find_video_source(&post.post));

        if let Some(source) = video_source {
            self.launch_external_video(source);
            return Ok(());
        }

        if !self.kitty_status.is_enabled() {
            self.status_message = "Inline previews are disabled in this terminal.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        if self.banner_selected() {
            self.status_message = "Select a post before toggling the preview.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        if select_preview_source(&post.post).is_none() {
            self.status_message = "This post has no inline preview.".to_string();
            self.mark_dirty();
            return Ok(());
        }

        let key = post.post.name.clone();
        self.media_fullscreen_prev_focus = Some(self.focused_pane);
        self.media_fullscreen_prev_scroll = Some(self.content_scroll);
        self.media_fullscreen = true;
        self.focused_pane = Pane::Content;
        self.content_scroll = 0;
        self.queue_active_kitty_delete();

        if let Some(cancel) = self.pending_media.remove(&key) {
            cancel.store(true, Ordering::SeqCst);
        }
        self.remove_pending_media_tracking(&key);
        if self
            .media_previews
            .get(&key)
            .is_some_and(|preview| preview.limited_cols() || preview.limited_rows())
        {
            self.media_previews.remove(&key);
        }
        self.media_failures.remove(&key);
        self.media_layouts.remove(&key);

        self.sync_content_from_selection();
        self.status_message = "Fullscreen preview enabled. Press f to return.".to_string();
        self.mark_dirty();
        Ok(())
    }

    fn request_media_preview(&mut self, post: &reddit::Post) {
        let key = post.name.clone();
        if self.pending_media.contains_key(&key)
            || self.media_previews.contains_key(&key)
            || self.media_failures.contains(&key)
        {
            return;
        }

        let (max_cols, max_rows) = self.media_constraints();
        self.request_media_preview_with_limits(post, max_cols, max_rows, false);
    }

    fn trim_pending_media_queue(&mut self, protected: &str) {
        if self.pending_media.len() < MAX_PENDING_MEDIA_REQUESTS {
            return;
        }
        let selected = self
            .posts
            .get(self.selected_post)
            .map(|post| post.post.name.clone());
        let mut skipped: Vec<String> = Vec::new();

        while self.pending_media.len() >= MAX_PENDING_MEDIA_REQUESTS {
            let Some(candidate) = self.pending_media_order.pop_front() else {
                break;
            };
            if candidate == protected || selected.as_ref().is_some_and(|name| name == &candidate) {
                skipped.push(candidate);
                if skipped.len() >= self.pending_media.len() {
                    break;
                }
                continue;
            }
            if let Some(flag) = self.pending_media.remove(&candidate) {
                flag.store(true, Ordering::SeqCst);
            }
        }

        for key in skipped {
            self.pending_media_order.push_back(key);
        }
    }

    fn remove_pending_media_tracking(&mut self, key: &str) {
        self.pending_media_order.retain(|entry| entry != key);
    }

    fn request_media_preview_with_limits(
        &mut self,
        post: &reddit::Post,
        max_cols: i32,
        max_rows: i32,
        allow_upscale: bool,
    ) {
        let key = post.name.clone();
        if self.pending_media.contains_key(&key) {
            return;
        }

        self.trim_pending_media_queue(&key);

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.pending_media.insert(key.clone(), cancel_flag.clone());
        self.pending_media_order.push_back(key.clone());

        let selected_name = self
            .posts
            .get(self.selected_post)
            .map(|post| post.post.name.clone());
        let priority = if selected_name.as_ref().is_some_and(|name| name == &key) {
            media::Priority::High
        } else {
            media::Priority::Normal
        };
        let tx = self.response_tx.clone();
        let post_clone = post.clone();
        let media_handle = self.media_handle.clone();
        let allow_inline_video = self.kitty_status.is_enabled();

        thread::spawn(move || {
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let name = post_clone.name.clone();
            let result = load_media_preview(
                &post_clone,
                cancel_flag.as_ref(),
                media_handle,
                max_cols,
                max_rows,
                allow_upscale,
                allow_inline_video,
                priority,
            );
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(AsyncResponse::Media {
                post_name: name,
                result,
            });
        });
    }

    fn media_constraints(&self) -> (i32, i32) {
        (self.media_constraints.cols, self.media_constraints.rows)
    }

    fn update_media_constraints(&mut self, area: Rect, fullscreen: bool) -> bool {
        let available_cols = if fullscreen {
            area.width.max(1) as i32
        } else {
            area.width.saturating_sub(MEDIA_INDENT).max(1) as i32
        };
        let available_rows = if fullscreen {
            area.height.max(1) as i32
        } else {
            area.height.saturating_sub(1).max(1) as i32
        };
        let cols = if fullscreen {
            available_cols.max(1)
        } else {
            MAX_IMAGE_COLS.min(available_cols.max(1))
        };
        let rows = if fullscreen {
            available_rows.max(1)
        } else {
            MAX_IMAGE_ROWS.min(available_rows.max(1))
        };
        let new_constraints = MediaConstraints { cols, rows };
        if new_constraints != self.media_constraints {
            self.media_constraints = new_constraints;
            if self.active_video.is_some() {
                self.needs_video_refresh = true;
            }
            true
        } else {
            false
        }
    }

    fn ensure_media_request_ready(&mut self, post: &PostPreview) {
        let key = post.post.name.clone();
        if self.media_failures.contains(&key)
            || self.media_previews.contains_key(&key)
            || self.pending_media.contains_key(&key)
        {
            return;
        }
        if !self.post_rows.contains_key(&key) {
            return;
        }
        if !self.content_cache.contains_key(&key) {
            return;
        }
        self.request_media_preview(&post.post);
    }

    fn load_comments_for_selection(&mut self) -> Result<()> {
        self.ensure_cache_scope();
        self.comment_sort_selected = false;
        let Some(service) = self.comment_service.clone() else {
            self.comments.clear();
            self.collapsed_comments.clear();
            self.visible_comment_indices.clear();
            self.comment_offset.set(0);
            self.comment_status = "Sign in to load comments.".to_string();
            self.pending_comments = None;
            self.close_action_menu(None);
            return Ok(());
        };

        let Some(post) = self.posts.get(self.selected_post) else {
            self.comments.clear();
            self.collapsed_comments.clear();
            self.visible_comment_indices.clear();
            self.comment_offset.set(0);
            self.comment_status = "Select a post to load comments.".to_string();
            self.pending_comments = None;
            self.close_action_menu(None);
            return Ok(());
        };

        let key = post.post.name.clone();
        let subreddit = post.post.subreddit.clone();
        let article = post.post.id.clone();
        let cache_key = CommentCacheKey::new(&key, self.comment_sort);
        if let Some(entry) = self.comment_cache.get(&cache_key) {
            if entry.scope == self.cache_scope && entry.fetched_at.elapsed() < COMMENT_CACHE_TTL {
                self.comments = entry.comments.clone();
                self.insert_post_root_comment_placeholder();
                self.collapsed_comments.clear();
                self.selected_comment = 0;
                self.comment_offset.set(0);
                self.rebuild_visible_comments_reset();
                let real_total = self
                    .comments
                    .iter()
                    .filter(|entry| !entry.is_post_root)
                    .count();
                if real_total == 0 {
                    self.comment_status = format!(
                        "No comments yet. (cached · sorted by {})",
                        comment_sort_label(self.comment_sort)
                    );
                } else {
                    let visible = self
                        .visible_comment_indices
                        .iter()
                        .filter(|idx| {
                            self.comments
                                .get(**idx)
                                .is_some_and(|entry| !entry.is_post_root)
                        })
                        .count();
                    if visible == real_total {
                        self.comment_status = format!(
                            "{real_total} comments loaded (cached · sorted by {})",
                            comment_sort_label(self.comment_sort)
                        );
                    } else {
                        let hidden = real_total.saturating_sub(visible);
                        self.comment_status = format!(
                            "{real_total} comments loaded (cached · sorted by {}) · {visible} visible · {hidden} hidden",
                            comment_sort_label(self.comment_sort)
                        );
                    }
                }
                self.pending_comments = None;
                self.close_action_menu(None);
                return Ok(());
            }
        }

        if let Some(pending) = self.pending_comments.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let post_name = key.clone();
        let sort = self.comment_sort;

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.pending_comments = Some(PendingComments {
            request_id,
            post_name: post_name.clone(),
            cancel_flag: cancel_flag.clone(),
            sort,
        });
        self.comment_status = format!(
            "Loading comments... · sorted by {}",
            comment_sort_label(sort)
        );
        self.comments.clear();
        self.collapsed_comments.clear();
        self.visible_comment_indices.clear();
        self.spinner.reset();
        self.close_action_menu(None);

        let tx = self.response_tx.clone();
        let service = service.clone();

        thread::spawn(move || {
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let result = service
                .load_comments(&subreddit, &article, sort)
                .map(|listing| {
                    let mut entries = Vec::new();
                    collect_comments(&listing.comments, 0, &mut entries);
                    entries
                });
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(AsyncResponse::Comments {
                request_id,
                post_name,
                sort,
                result,
            });
        });
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let full = frame.size();
        self.terminal_cols = full.width.max(1);
        self.terminal_rows = full.height.max(1);
        frame.render_widget(Block::default().style(Style::default().bg(COLOR_BG)), full);

        if self.media_fullscreen {
            self.draw_content(frame, full);
            return;
        }

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(frame.size());

        let raw_status = if self.is_loading() {
            format!("{} {}", self.spinner.frame(), self.status_message)
                .trim()
                .to_string()
        } else {
            self.status_message.clone()
        };
        let version_status = format!("Reddix {}", self.version_summary());
        let mut status_parts: Vec<String> = Vec::new();
        if !raw_status.is_empty() {
            status_parts.push(raw_status);
        }
        if !self.show_nsfw {
            status_parts.push("NSFW hidden".to_string());
        }
        status_parts.push(version_status);
        let status_text = status_parts.join(" · ");
        let status_line = Paragraph::new(status_text).style(
            Style::default()
                .fg(COLOR_TEXT_PRIMARY)
                .bg(COLOR_PANEL_FOCUSED_BG)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_widget(status_line, layout[0]);

        if self.media_fullscreen {
            self.draw_content(frame, layout[1]);
        } else {
            let window = self.visible_panes();
            let constraints = pane_constraints(&window);
            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(constraints)
                .split(layout[1]);

            for (pane, area) in window.iter().zip(main_chunks.iter()) {
                match pane {
                    Pane::Navigation => self.draw_subreddits(frame, *area),
                    Pane::Posts => self.draw_posts(frame, *area),
                    Pane::Content => self.draw_content(frame, *area),
                    Pane::Comments => self.draw_comments(frame, *area),
                }
            }
        }

        let footer = Paragraph::new(self.footer_text())
            .style(
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::ITALIC),
            )
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        frame.render_widget(footer, layout[2]);

        if self.menu_visible {
            self.draw_menu(frame, layout[1]);
        }

        if self.action_menu_visible {
            self.draw_action_menu(frame, layout[1]);
        }

        if self.help_visible {
            self.draw_help_overlay(frame, layout[1]);
        }

        if self.comment_composer.is_some() {
            self.draw_comment_composer(frame, layout[1]);
        }
    }

    fn resolve_media_origin(&self, layout: MediaLayout) -> Option<MediaOrigin> {
        let area = self.content_area?;
        let content_width = area.width.saturating_sub(layout.indent).max(1);
        let lines = &self.content.lines;
        let line_offset = layout.line_offset.min(lines.len());
        let scroll_lines = self.content_scroll as usize;
        let visual_offset = visual_height(&lines[..line_offset], content_width);
        let visual_scroll = visual_height(&lines[..scroll_lines.min(lines.len())], content_width);

        if visual_offset < visual_scroll {
            return None;
        }

        let relative_row = visual_offset - visual_scroll;
        if relative_row >= area.height as usize {
            return None;
        }

        Some(MediaOrigin {
            row: area.y + relative_row as u16,
            col: area.x.saturating_add(layout.indent),
            visual_scroll,
            visual_offset,
        })
    }

    fn flush_inline_clears(&mut self, backend: &mut CrosstermBackend<Stdout>) -> Result<()> {
        self.flush_pending_kitty_deletes(backend)?;

        if let Some((_row, _col, _cols, _rows)) = self.pending_video_clear.take() {
            let delete_seq = kitty_delete_all_sequence();
            crossterm::queue!(backend, Print(delete_seq))?;
            for preview in self.media_previews.values_mut() {
                if let Some(kitty) = preview.kitty_mut() {
                    kitty.transmitted = false;
                }
            }
            self.active_kitty = None;
            self.needs_kitty_flush = true;
            backend.flush()?;
            self.needs_redraw = true;
        }

        Ok(())
    }

    fn flush_inline_images(&mut self, backend: &mut CrosstermBackend<Stdout>) -> Result<()> {
        if self.action_menu_visible
            || self.menu_visible
            || self.help_visible
            || self.comment_composer.is_some()
        {
            self.needs_kitty_flush = true;
            self.emit_active_kitty_delete(backend)?;
            return Ok(());
        }

        if !self.needs_kitty_flush {
            return Ok(());
        }
        self.needs_kitty_flush = false;

        let mut requested_redraw = false;

        let Some(area) = self.content_area else {
            let _ = self.stop_active_video(None, true);
            self.emit_active_kitty_delete(backend)?;
            return Ok(());
        };
        let Some(post) = self.posts.get(self.selected_post) else {
            let _ = self.stop_active_video(None, true);
            self.emit_active_kitty_delete(backend)?;
            return Ok(());
        };
        let post_name = post.post.name.clone();

        if self
            .active_kitty
            .as_ref()
            .is_some_and(|active| active.post_name != post_name)
        {
            self.emit_active_kitty_delete(backend)?;
        }
        if !self.media_previews.contains_key(&post_name) {
            if self.active_kitty_matches(&post_name) {
                self.emit_active_kitty_delete(backend)?;
            }
            let _ = self.stop_active_video(None, true);
            return Ok(());
        }
        let Some(layout) = self.media_layouts.get(&post_name).copied() else {
            if self.active_kitty_matches(&post_name) {
                self.emit_active_kitty_delete(backend)?;
            }
            let _ = self.stop_active_video(None, true);
            return Ok(());
        };

        let active_matches = self.active_kitty_matches(&post_name);

        let Some(origin) = self.resolve_media_origin(layout) else {
            if active_matches {
                self.emit_active_kitty_delete(backend)?;
            }
            return Ok(());
        };

        let row = origin.row;
        let col = origin.col;

        if self.active_kitty.as_ref().is_some_and(|active| {
            active.post_name == post_name && (active.row != row || active.col != col)
        }) {
            self.emit_active_kitty_delete(backend)?;
        }

        if let Some(preview_ref) = self.media_previews.get(&post_name) {
            if preview_ref.has_video() {
                if active_matches {
                    self.emit_active_kitty_delete(backend)?;
                }
                if self.active_video.is_none() {
                    self.needs_video_refresh = true;
                }
                return Ok(());
            }
            if !preview_ref.has_kitty() {
                if active_matches {
                    self.emit_active_kitty_delete(backend)?;
                }
                let _ = self.stop_active_video(None, true);
                return Ok(());
            }
        }

        let Some(preview) = self.media_previews.get_mut(&post_name) else {
            if active_matches {
                self.emit_active_kitty_delete(backend)?;
            }
            let _ = self.stop_active_video(None, true);
            return Ok(());
        };
        let Some(kitty) = preview.kitty_mut() else {
            if active_matches {
                self.emit_active_kitty_delete(backend)?;
            }
            let _ = self.stop_active_video(None, true);
            return Ok(());
        };

        if kitty_debug_enabled() {
            eprintln!(
                "kitty_debug: post={} col={} row={} cols={} rows={} area=({},{} {}x{}) scroll={} line_offset={} indent={} content_scroll={}",
                post_name,
                col,
                row,
                kitty.cols,
                kitty.rows,
                area.x,
                area.y,
                area.width,
                area.height,
                origin.visual_scroll,
                origin.visual_offset,
                layout.indent,
                self.content_scroll
            );
        }

        let was_transmitted = kitty.transmitted;
        kitty.ensure_transmitted(backend)?;
        let sequence = kitty.placement_sequence();
        crossterm::queue!(backend, MoveTo(col, row), Print(sequence))?;
        backend.flush()?;

        if !was_transmitted {
            requested_redraw = true;
        }

        self.active_kitty = Some(ActiveKitty {
            post_name,
            image_id: kitty.id,
            wrap_tmux: kitty.wrap_tmux,
            row,
            col,
        });

        if requested_redraw {
            self.needs_redraw = true;
        }

        Ok(())
    }

    fn cleanup_inline_media(&mut self, backend: &mut CrosstermBackend<Stdout>) -> Result<()> {
        if let Some(pending) = self.pending_posts.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        if let Some(pending) = self.pending_comments.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        if let Some(pending) = self.pending_content.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
        self.pending_post_rows = None;
        self.pending_subreddits = None;
        self.cancel_pending_video();
        let _ = self.stop_active_video(None, true);
        for flag in self.pending_media.values() {
            flag.store(true, Ordering::SeqCst);
        }
        self.pending_media.clear();
        self.pending_media_order.clear();
        self.flush_inline_clears(backend)?;
        self.emit_active_kitty_delete(backend)
            .context("cleanup inline media: emit active kitty delete")?;
        self.flush_pending_kitty_deletes(backend)
            .context("cleanup inline media: flush pending kitty deletes")?;
        let delete_seq = kitty_delete_all_sequence();
        crossterm::queue!(backend, Print(delete_seq))
            .context("cleanup inline media: queue global delete")?;
        backend
            .flush()
            .context("cleanup inline media: flush terminal backend")?;
        self.pending_video_clear = None;
        self.pending_kitty_deletes.clear();
        self.needs_kitty_flush = false;
        self.active_kitty = None;
        self.pending_video = None;
        self.pending_external_video = None;
        Ok(())
    }

    fn cancel_pending_video(&mut self) {
        if let Some(pending) = self.pending_video.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }
    }

    fn pending_video_matches(
        &self,
        post_name: &str,
        origin: MediaOrigin,
        dims: (i32, i32),
        source: &video::VideoSource,
    ) -> bool {
        self.pending_video.as_ref().is_some_and(|pending| {
            pending.post_name == post_name
                && pending.origin.row == origin.row
                && pending.origin.col == origin.col
                && pending.dims == dims
                && pending.source.playback_url == source.playback_url
        })
    }

    fn launch_inline_video(
        &mut self,
        post_name: String,
        origin: MediaOrigin,
        source: video::VideoSource,
        dims: (i32, i32),
        playback: String,
        streaming: bool,
    ) -> Result<()> {
        let mpv_path = env::var(MPV_PATH_ENV).unwrap_or_else(|_| "mpv".to_string());
        let term_cols = i32::from(self.terminal_cols.max(1));
        let term_rows = i32::from(self.terminal_rows.max(1));
        let metrics = terminal_cell_metrics();
        let cell_width = metrics.width.max(1.0);
        let cell_height = metrics.height.max(1.0);
        let preview_cols = dims.0.max(1);
        let preview_rows = dims.1.max(1);
        let pixel_width = ((preview_cols as f64) * cell_width).round() as i32;
        let pixel_height = ((preview_rows as f64) * cell_height).round() as i32;
        let launch = video::InlineLaunchOptions {
            mpv_path: &mpv_path,
            source: &source,
            playback: Cow::Owned(playback),
            cols: preview_cols,
            rows: preview_rows,
            col: origin.col,
            row: origin.row,
            term_cols,
            term_rows,
            pixel_width,
            pixel_height,
        };

        let session = video::spawn_inline_player(launch)?;
        let controls_supported = session.controls_supported();
        self.active_video = Some(ActiveVideo {
            session,
            source,
            post_name,
            row: origin.row,
            col: origin.col,
            cols: dims.0.max(1),
            rows: dims.1.max(1),
        });
        video::debug_log(format!(
            "inline launch row={} col={} cols={} rows={}",
            origin.row, origin.col, dims.0, dims.1
        ));
        self.pending_video = None;
        self.needs_video_refresh = false;
        let controls_hint = if controls_supported {
            "space/p pause · [ ] seek ±5s · Esc stop"
        } else {
            "Esc stop (inline controls unavailable on this platform)"
        };
        self.status_message = if streaming {
            format!("Streaming inline video preview — {}.", controls_hint)
        } else {
            format!("Playing inline video preview — {}.", controls_hint)
        };
        self.mark_dirty();
        Ok(())
    }

    fn start_inline_video(
        &mut self,
        post_name: String,
        origin: MediaOrigin,
        source: video::VideoSource,
        dims: (i32, i32),
    ) -> Result<()> {
        self.cancel_pending_video();
        self.video_completed_post = None;
        let display_label = if source.label.trim().is_empty() {
            "video".to_string()
        } else {
            source.label.trim().to_string()
        };
        if let Some(handle) = self.media_handle.clone() {
            let playback_url = source.playback_url.clone();
            let request_id = self.next_request_id;
            self.next_request_id = self.next_request_id.saturating_add(1);
            let cancel_flag = Arc::new(AtomicBool::new(false));
            self.pending_video = Some(PendingVideo {
                request_id,
                post_name: post_name.clone(),
                source,
                origin,
                dims,
                cancel_flag: cancel_flag.clone(),
            });
            self.status_message = format!("Downloading video preview for \"{}\"…", display_label);
            self.needs_video_refresh = false;
            self.mark_dirty();
            let tx = self.response_tx.clone();
            thread::spawn(move || {
                if cancel_flag.load(Ordering::SeqCst) {
                    return;
                }
                let result = fetch_cached_video_path(&handle, &playback_url, media::Priority::High)
                    .map(|path| path.to_string_lossy().to_string());
                if cancel_flag.load(Ordering::SeqCst) {
                    return;
                }
                let _ = tx.send(AsyncResponse::InlineVideo {
                    request_id,
                    post_name,
                    result,
                });
            });
            return Ok(());
        }

        let playback = source.playback_url.clone();
        self.launch_inline_video(post_name, origin, source, dims, playback, true)
    }

    fn launch_external_video(&mut self, source: video::VideoSource) {
        let label = if source.label.trim().is_empty() {
            "video".to_string()
        } else {
            source.label.trim().to_string()
        };
        let mpv_path = env::var(MPV_PATH_ENV).unwrap_or_else(|_| "mpv".to_string());
        let playback_url = source.playback_url.clone();
        let handle = self.media_handle.clone();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.pending_external_video = Some(request_id);
        self.status_message = format!("Launching fullscreen player for \"{}\"…", label);
        self.mark_dirty();
        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let mut playback_target = playback_url.clone();
            if let Some(handle) = handle.as_ref() {
                match fetch_cached_video_path(handle, &playback_url, media::Priority::High) {
                    Ok(path) => {
                        playback_target = path.to_string_lossy().to_string();
                        video::debug_log(format!(
                            "external video cache hit url={} path={}",
                            playback_url,
                            path.display()
                        ));
                    }
                    Err(err) => {
                        video::debug_log(format!(
                            "external video cache fetch failed for {}: {}",
                            playback_url, err
                        ));
                    }
                }
            }
            let mut result = video::spawn_external_player(ExternalLaunchOptions {
                mpv_path: &mpv_path,
                source: &source,
                playback: &playback_target,
                fullscreen: true,
            });
            if result.is_err() && playback_target != playback_url {
                video::debug_log(format!(
                    "retrying external video launch with remote url {}",
                    playback_url
                ));
                result = video::spawn_external_player(ExternalLaunchOptions {
                    mpv_path: &mpv_path,
                    source: &source,
                    playback: &playback_url,
                    fullscreen: true,
                });
            }
            let _ = tx.send(AsyncResponse::ExternalVideo {
                request_id,
                label,
                result,
            });
        });
    }

    fn stop_active_video(&mut self, message: Option<&str>, silent: bool) -> bool {
        self.cancel_pending_video();
        if let Some(active) = self.active_video.take() {
            let ActiveVideo {
                session,
                source,
                post_name,
                row,
                col,
                cols,
                rows,
            } = active;

            self.pending_video_clear = Some((row, col, cols, rows));
            self.video_completed_post = Some(post_name);

            let outcome = session.stop_blocking();

            if !silent {
                if let Some(msg) = message {
                    self.status_message = msg.to_string();
                } else if let Some(result) = outcome {
                    self.status_message = match result {
                        Ok(status) => {
                            if status.success() {
                                if source.label.trim().is_empty() {
                                    "Video playback finished.".to_string()
                                } else {
                                    format!("Finished playing \"{}\".", source.label.trim())
                                }
                            } else if let Some(code) = status.code() {
                                format!("Video playback ended with status {}.", code)
                            } else {
                                "Video playback ended unexpectedly.".to_string()
                            }
                        }
                        Err(err) => format!("Video playback error: {}", err),
                    };
                }
            } else if let Some(msg) = message {
                self.status_message = msg.to_string();
            }

            self.needs_video_refresh = false;
            self.needs_kitty_flush = true;
            self.mark_dirty();
            true
        } else {
            if !silent {
                if let Some(msg) = message {
                    self.status_message = msg.to_string();
                }
            }
            false
        }
    }

    fn poll_active_video(&mut self) {
        let mut finished: Option<VideoCompletion> = None;
        if let Some(active) = self.active_video.as_mut() {
            if let Some(result) = active.try_status() {
                finished = Some(VideoCompletion {
                    post_name: active.post_name.clone(),
                    result,
                    label: active.source.label.clone(),
                    row: active.row,
                    col: active.col,
                    cols: active.cols,
                    rows: active.rows,
                });
            }
        }

        if let Some(VideoCompletion {
            post_name,
            result,
            label,
            row,
            col,
            cols,
            rows,
        }) = finished
        {
            self.active_video = None;
            self.pending_video_clear = Some((row, col, cols, rows));
            self.status_message = match result {
                Ok(status) => {
                    if status.success() {
                        if label.trim().is_empty() {
                            "Video playback finished.".to_string()
                        } else {
                            format!("Finished playing \"{}\".", label.trim())
                        }
                    } else if let Some(code) = status.code() {
                        format!("Video playback ended with status {}.", code)
                    } else {
                        "Video playback ended unexpectedly.".to_string()
                    }
                }
                Err(err) => format!("Video playback error: {}", err),
            };
            self.needs_video_refresh = false;
            self.needs_kitty_flush = true;
            self.video_completed_post = Some(post_name);
            self.mark_dirty();
        }
    }

    fn refresh_inline_video(&mut self) -> Result<()> {
        if !self.kitty_status.is_enabled() {
            let _ = self.stop_active_video(
                Some("Inline video requires Kitty previews; playback stopped."),
                false,
            );
            self.needs_video_refresh = false;
            return Ok(());
        }

        if self.action_menu_visible
            || self.menu_visible
            || self.help_visible
            || self.media_fullscreen
        {
            let _ = self.stop_active_video(None, true);
            self.needs_video_refresh = true;
            return Ok(());
        }

        if !self.needs_video_refresh && self.active_video.is_some() {
            return Ok(());
        }

        let (post_name, dims, video_source, origin) = {
            let Some(post) = self.posts.get(self.selected_post) else {
                let _ = self.stop_active_video(None, true);
                self.needs_video_refresh = true;
                return Ok(());
            };
            let preview_ref = match self.media_previews.get(&post.post.name) {
                Some(preview) => preview,
                None => {
                    let _ = self.stop_active_video(None, true);
                    self.needs_video_refresh = true;
                    return Ok(());
                }
            };
            let (dims, video_source) = match preview_ref.video() {
                Some(video_preview) => (preview_ref.dims(), video_preview.source.clone()),
                None => {
                    let _ = self.stop_active_video(None, true);
                    self.needs_video_refresh = false;
                    return Ok(());
                }
            };
            let Some(layout) = self.media_layouts.get(&post.post.name).copied() else {
                let _ = self.stop_active_video(None, true);
                self.needs_video_refresh = true;
                return Ok(());
            };
            let Some(origin) = self.resolve_media_origin(layout) else {
                let _ = self.stop_active_video(None, true);
                self.needs_video_refresh = true;
                return Ok(());
            };
            (post.post.name.clone(), dims, video_source, origin)
        };

        if let Some(active) = &self.active_video {
            if active.matches_geometry(origin.row, origin.col, dims.0, dims.1, &video_source) {
                self.needs_video_refresh = false;
                return Ok(());
            }
        }

        if self.pending_video_matches(&post_name, origin, dims, &video_source) {
            self.needs_video_refresh = false;
            return Ok(());
        }

        if !self.needs_video_refresh
            && self
                .video_completed_post
                .as_deref()
                .is_some_and(|completed| completed == post_name)
        {
            return Ok(());
        }

        let _ = self.stop_active_video(None, true);
        if let Err(err) = self.start_inline_video(post_name.clone(), origin, video_source, dims) {
            self.status_message = format!("Failed to start video preview: {}", err);
            self.needs_video_refresh = false;
            self.mark_dirty();
        }
        Ok(())
    }

    fn restart_inline_video(&mut self) -> Result<()> {
        self.stop_active_video(None, true);
        self.needs_video_refresh = true;
        self.refresh_inline_video()
    }

    fn pane_block(&self, pane: Pane) -> Block<'static> {
        let focused = self.focused_pane == pane;
        let border_style = if focused {
            Style::default().fg(COLOR_BORDER_FOCUSED)
        } else {
            Style::default().fg(COLOR_BORDER_IDLE)
        };
        let title_style = if focused {
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_TEXT_SECONDARY)
        };
        let title_text = if pane == Pane::Content && self.media_fullscreen {
            "Media Preview (fullscreen)"
        } else {
            pane.title()
        };
        Block::default()
            .title(Span::styled(title_text, title_style))
            .borders(Borders::ALL)
            .border_style(border_style)
            .style(Style::default().bg(COLOR_PANEL_BG))
            .padding(Padding::uniform(1))
    }

    fn sort_lines_for_width(&self, width: usize, focused: bool) -> Vec<Line<'static>> {
        let is_selected = focused && matches!(self.nav_mode, NavMode::Sorts);
        let spacing_style = if is_selected {
            Style::default()
                .bg(COLOR_PANEL_SELECTED_BG)
                .fg(COLOR_TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_TEXT_SECONDARY)
        };

        let mut entries = Vec::with_capacity(NAV_SORTS.len());
        for (idx, sort) in NAV_SORTS.iter().enumerate() {
            let is_active = self.sort == *sort;
            let mut style = Style::default().fg(if is_active {
                COLOR_ACCENT
            } else {
                COLOR_TEXT_SECONDARY
            });
            if is_selected {
                style = style
                    .add_modifier(Modifier::BOLD)
                    .bg(COLOR_PANEL_SELECTED_BG)
                    .fg(COLOR_TEXT_PRIMARY);
            }
            let marker = if is_active { "●" } else { "○" };
            let number = idx + 1;
            let label = format!("{number} {marker} {}", sort_label(*sort));
            let label_width = UnicodeWidthStr::width(label.as_str());
            entries.push((label, style, label_width));
        }

        let available_width = width.max(1);
        let mut lines: Vec<Vec<Span>> = Vec::new();
        let mut current_line: Vec<Span> = Vec::new();
        let mut current_width = 0usize;

        for (label, style, label_width) in entries.into_iter() {
            let spacing_width = if current_line.is_empty() { 0 } else { 2 };
            if !current_line.is_empty()
                && current_width + spacing_width + label_width > available_width
            {
                lines.push(current_line);
                current_line = Vec::new();
                current_width = 0;
            }
            if !current_line.is_empty() {
                current_line.push(Span::styled("  ".to_string(), spacing_style));
                current_width += spacing_width;
            }
            current_line.push(Span::styled(label, style));
            current_width += label_width;
        }

        if current_line.is_empty() {
            current_line.push(Span::raw(""));
        }
        lines.push(current_line);

        let mut output = Vec::with_capacity(lines.len() + 1);
        output.push(Line::from(vec![Span::styled(
            "Sort",
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .add_modifier(Modifier::BOLD),
        )]));
        output.extend(lines.into_iter().map(Line::from));
        output
    }

    fn comment_sort_lines_for_width(&self, width: usize, focused: bool) -> Vec<Line<'static>> {
        let mut entries: Vec<(String, Style, usize)> = Vec::with_capacity(COMMENT_SORTS.len());
        for (idx, sort) in COMMENT_SORTS.iter().enumerate() {
            let is_active = self.comment_sort == *sort;
            let marker = if is_active { "●" } else { "○" };
            let mut style = if is_active {
                if focused {
                    Style::default()
                        .bg(COLOR_PANEL_SELECTED_BG)
                        .fg(COLOR_TEXT_PRIMARY)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD)
                }
            } else if focused {
                Style::default()
                    .bg(COLOR_PANEL_SELECTED_BG)
                    .fg(COLOR_TEXT_PRIMARY)
            } else {
                Style::default().fg(COLOR_TEXT_SECONDARY)
            };
            if !focused {
                style = style.bg(COLOR_PANEL_BG);
            }
            let number = idx + 1;
            let label = format!("{number} {marker} {}", comment_sort_label(*sort));
            let label_width = UnicodeWidthStr::width(label.as_str());
            entries.push((label, style, label_width));
        }

        let available_width = width.max(1);
        let mut lines: Vec<Vec<Span>> = Vec::new();
        let mut current_line: Vec<Span> = Vec::new();
        let mut current_width = 0usize;

        for (label, style, label_width) in entries.into_iter() {
            let spacing_width = if current_line.is_empty() { 0 } else { 3 };
            if !current_line.is_empty()
                && current_width + spacing_width + label_width > available_width
            {
                lines.push(current_line);
                current_line = Vec::new();
                current_width = 0;
            }
            if !current_line.is_empty() {
                current_line.push(Span::raw("   "));
                current_width += spacing_width;
            }
            current_line.push(Span::styled(label, style));
            current_width += label_width;
        }

        if current_line.is_empty() {
            current_line.push(Span::raw(""));
        }
        lines.push(current_line);

        lines.into_iter().map(Line::from).collect()
    }

    fn draw_subreddits(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = self.pane_block(Pane::Navigation);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let focused = self.focused_pane == Pane::Navigation;

        let available_width = inner.width.max(1) as usize;
        let sort_lines = self.sort_lines_for_width(available_width, focused);
        let sort_height = sort_lines.len() as u16;

        let instructions_height = if inner.height > sort_height + 2 { 2 } else { 0 };

        let mut constraints = Vec::with_capacity(3);
        constraints.push(Constraint::Length(sort_height));
        if instructions_height > 0 {
            constraints.push(Constraint::Length(instructions_height));
        }
        constraints.push(Constraint::Min(0));

        let layout_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);

        let sort_area = layout_chunks[0];
        let (instructions_area, list_area) = if instructions_height > 0 {
            (Some(layout_chunks[1]), layout_chunks[2])
        } else {
            (None, layout_chunks[1])
        };

        let sorts_paragraph = Paragraph::new(Text::from(sort_lines))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(sorts_paragraph, sort_area);

        if let Some(area) = instructions_area {
            let instructions = Paragraph::new(Text::from(vec![
                Line::from(vec![Span::styled(
                    "Controls",
                    Style::default()
                        .fg(COLOR_TEXT_PRIMARY)
                        .add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::styled(
                    "h/l or ←/→ switch panes · j/k move within the list (press k on first row to reach sort) · digits/Enter load selection",
                Style::default().fg(COLOR_TEXT_SECONDARY),
            )]),
        ]))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true });
            frame.render_widget(instructions, area);
        }

        self.subreddit_view_height.set(list_area.height);
        self.subreddit_view_width.set(list_area.width);
        self.ensure_subreddit_visible();

        let width = list_area.width.max(1) as usize;
        let max_visible_height = list_area.height as usize;
        let offset = self.subreddit_offset.get().min(self.subreddits.len());
        let mut used_height = 0usize;
        let mut items: Vec<ListItem> = Vec::with_capacity(self.subreddits.len().max(1));
        for (idx, name) in self.subreddits.iter().enumerate().skip(offset) {
            let is_selected =
                focused && matches!(self.nav_mode, NavMode::Subreddits) && self.nav_index == idx;
            let is_active = self.selected_sub == idx;
            let background = if is_selected {
                COLOR_PANEL_SELECTED_BG
            } else {
                COLOR_PANEL_BG
            };
            let mut style = Style::default()
                .fg(if is_selected || is_active {
                    COLOR_TEXT_PRIMARY
                } else {
                    COLOR_TEXT_SECONDARY
                })
                .bg(background);
            if is_selected || is_active {
                style = style.add_modifier(Modifier::BOLD);
            }
            let display = navigation_display_name(name);
            let mut lines = wrap_plain(&display, width, style);
            lines.push(Line::from(Span::styled(
                String::new(),
                Style::default().bg(background),
            )));
            pad_lines_to_width(&mut lines, list_area.width);
            used_height = used_height.saturating_add(lines.len());
            items.push(ListItem::new(lines));
            if max_visible_height > 0 && used_height >= max_visible_height {
                break;
            }
        }

        if items.is_empty() {
            let mut lines = vec![Line::from(Span::styled(
                "No subreddits",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::ITALIC),
            ))];
            pad_lines_to_width(&mut lines, list_area.width);
            items.push(ListItem::new(lines));
        }

        let list = List::new(items);
        frame.render_widget(list, list_area);
    }

    fn draw_posts(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = self.pane_block(Pane::Posts);
        let inner = block.inner(area);
        let width = inner.width.max(1) as usize;
        let pane_width = inner.width;
        self.post_view_height.set(inner.height);
        self.ensure_post_visible();
        let offset = self.post_offset.get().min(self.posts.len());

        let mut score_width = 0usize;
        let mut comments_width = 0usize;
        for item in &self.posts {
            score_width = score_width.max(item.post.score.to_string().chars().count());
            comments_width = comments_width.max(item.post.num_comments.to_string().chars().count());
        }
        score_width = score_width.max(3);
        comments_width = comments_width.max(2);

        let loading_posts = self.pending_posts.is_some();
        let mut items: Vec<ListItem> = Vec::new();
        if let Some(update) = &self.update_notice {
            let installing = self.update_install_in_progress;
            let message = if installing {
                format!(
                    "Installing update: {} → {} (GitHub Releases)",
                    self.current_version, update.version
                )
            } else {
                format!(
                    "Update available: {} → {} (GitHub Releases)",
                    self.current_version, update.version
                )
            };
            let focused = self.focused_pane == Pane::Posts;
            let selected = self.banner_selected();
            let highlight = focused && selected;
            let background = if highlight {
                COLOR_PANEL_SELECTED_BG
            } else {
                COLOR_PANEL_BG
            };
            let mut line_style = Style::default()
                .fg(COLOR_ACCENT)
                .bg(background)
                .add_modifier(Modifier::BOLD);
            if installing {
                line_style = line_style.add_modifier(Modifier::ITALIC);
            }
            let mut lines = vec![Line::from(Span::styled(message, line_style))];
            let detail_text = if installing {
                "Installer running… you can keep browsing while it finishes."
            } else if self.update_install_finished {
                "Update installed. Restart Reddix to use the new version."
            } else if highlight {
                "Press Enter to install now · Shift+U works anywhere."
            } else {
                "Select and press Enter to install."
            };
            let mut detail_style = Style::default().bg(background);
            detail_style = detail_style.fg(if highlight {
                COLOR_TEXT_PRIMARY
            } else {
                COLOR_TEXT_SECONDARY
            });
            detail_style = detail_style.add_modifier(Modifier::ITALIC);
            if installing {
                detail_style = detail_style.fg(COLOR_ACCENT);
            }
            lines.push(Line::from(Span::styled(
                detail_text.to_string(),
                detail_style,
            )));
            lines.push(Line::from(Span::styled(
                String::new(),
                Style::default().bg(background),
            )));
            pad_lines_to_width(&mut lines, pane_width);
            items.push(ListItem::new(lines));
        }
        let remaining_height = if self.banner_selected() {
            self.available_post_height(offset.saturating_sub(1))
        } else {
            self.available_post_height(offset)
        };

        self.prepare_post_rows(width, score_width, comments_width);

        if loading_posts && offset == 0 && !self.posts.is_empty() {
            let mut header_lines = Vec::new();
            header_lines.push(Line::from(Span::styled(
                format!("{} Loading new posts…", self.spinner.frame()),
                Style::default()
                    .fg(COLOR_ACCENT)
                    .bg(COLOR_PANEL_BG)
                    .add_modifier(Modifier::BOLD),
            )));
            header_lines.push(Line::from(Span::styled(
                String::new(),
                Style::default().bg(COLOR_PANEL_BG),
            )));
            pad_lines_to_width(&mut header_lines, pane_width);
            items.push(ListItem::new(header_lines));
        }

        let mut used_height = 0usize;
        for (idx, item) in self.posts.iter().enumerate().skip(offset) {
            let focused = self.focused_pane == Pane::Posts;
            let selected = idx == self.selected_post && !self.banner_selected();
            let highlight = focused && selected;
            let background = if highlight {
                COLOR_PANEL_SELECTED_BG
            } else {
                COLOR_PANEL_BG
            };

            let primary_color = if highlight {
                COLOR_ACCENT
            } else if focused || selected {
                COLOR_TEXT_PRIMARY
            } else {
                COLOR_TEXT_SECONDARY
            };
            let identity_style = Style::default().fg(primary_color).bg(background);
            let mut title_style = Style::default()
                .fg(if focused {
                    COLOR_TEXT_PRIMARY
                } else {
                    COLOR_TEXT_SECONDARY
                })
                .bg(background);
            if selected && !focused {
                title_style = title_style.fg(COLOR_TEXT_PRIMARY);
            }
            if highlight {
                title_style = title_style.add_modifier(Modifier::BOLD);
            }
            let metrics_style = Style::default().fg(primary_color).bg(background);

            let post_name = &item.post.name;
            let mut push_item = |mut lines: Vec<Line<'static>>| {
                let item_height = lines.len().saturating_add(1).max(1);
                if remaining_height > 0
                    && used_height + item_height > remaining_height
                    && !items.is_empty()
                {
                    return false;
                }
                lines.push(Line::from(Span::styled(
                    String::new(),
                    Style::default().bg(background),
                )));
                pad_lines_to_width(&mut lines, pane_width);
                used_height = used_height.saturating_add(item_height.min(remaining_height));
                items.push(ListItem::new(lines));
                if remaining_height == 0 {
                    return false;
                }
                if remaining_height > 0 && used_height >= remaining_height {
                    return false;
                }
                true
            };

            if let Some(row) = self.post_rows.get(post_name) {
                let mut lines: Vec<Line<'static>> = Vec::new();
                let mut identity_lines = restyle_lines(&row.identity, identity_style);
                lines.append(&mut identity_lines);

                let mut title_lines = restyle_lines(&row.title, title_style);
                lines.append(&mut title_lines);

                let mut metrics_lines = restyle_lines(&row.metrics, metrics_style);
                lines.append(&mut metrics_lines);
                if !push_item(lines) {
                    break;
                }
            } else {
                let mut lines: Vec<Line<'static>> = Vec::new();
                lines.push(Line::from(Span::styled(
                    format!("{} Formatting post…", self.spinner.frame()),
                    Style::default()
                        .fg(if highlight || focused {
                            COLOR_TEXT_PRIMARY
                        } else {
                            COLOR_TEXT_SECONDARY
                        })
                        .bg(background),
                )));
                lines.push(Line::from(Span::styled(item.title.clone(), title_style)));
                if !push_item(lines) {
                    break;
                }
            }
        }

        if items.is_empty() {
            if loading_posts {
                let mut lines = vec![Line::from(Span::styled(
                    format!("{} Loading feed...", self.spinner.frame()),
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .bg(COLOR_PANEL_BG)
                        .add_modifier(Modifier::BOLD),
                ))];
                pad_lines_to_width(&mut lines, pane_width);
                items.push(ListItem::new(lines));
            } else {
                let mut lines = vec![Line::from(Span::styled(
                    "No posts loaded yet.",
                    Style::default()
                        .fg(COLOR_TEXT_SECONDARY)
                        .bg(COLOR_PANEL_BG)
                        .add_modifier(Modifier::ITALIC),
                ))];
                pad_lines_to_width(&mut lines, pane_width);
                items.push(ListItem::new(lines));
            }
        }

        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }

    fn prepare_post_rows(&mut self, width: usize, score_width: usize, comments_width: usize) {
        if width == 0 {
            return;
        }

        let width_changed = width != self.post_rows_width;
        if width_changed {
            self.post_rows.clear();
            self.pending_post_rows = None;
            self.post_rows_width = width;
        }

        if self.posts.is_empty() {
            self.pending_post_rows = None;
            return;
        }

        let mut inputs: Vec<PostRowInput> = Vec::new();
        for post in &self.posts {
            let name = post.post.name.clone();
            if !width_changed && self.post_rows.contains_key(&name) {
                continue;
            }
            inputs.push(PostRowInput {
                name,
                title: post.title.clone(),
                subreddit: post.post.subreddit.clone(),
                author: post.post.author.clone(),
                score: post.post.score,
                comments: post.post.num_comments,
                vote: match post.post.likes {
                    Some(true) => 1,
                    Some(false) => -1,
                    None => 0,
                },
            });
        }

        if inputs.is_empty() {
            return;
        }

        if let Some(pending) = &self.pending_post_rows {
            if pending.width == width {
                return;
            }
        }

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.pending_post_rows = Some(PendingPostRows { request_id, width });

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            let mut rows = Vec::with_capacity(inputs.len());
            for input in inputs {
                let data = build_post_row_data(&input, width, score_width, comments_width);
                rows.push((input.name, data));
            }
            let _ = tx.send(AsyncResponse::PostRows {
                request_id,
                width,
                rows,
            });
        });
    }

    fn compose_content(&mut self, base: Text<'static>, post: &PostPreview) -> Text<'static> {
        let key = post.post.name.clone();
        let mut lines = base.lines;
        self.media_layouts.remove(&key);
        let (max_cols, max_rows) = self.media_constraints();
        if let Some(preview) = self.media_previews.get(&key).cloned() {
            let (cols, rows) = preview.dims();
            let needs_downscale = preview.has_kitty() && (cols > max_cols || rows > max_rows);
            let limited_cols = preview.limited_cols();
            let limited_rows = preview.limited_rows();
            let expand_cols = limited_cols && max_cols > cols;
            let expand_rows = limited_rows && max_rows > rows;
            let can_expand = (expand_cols || expand_rows)
                && max_cols >= MIN_IMAGE_COLS
                && max_rows >= MIN_IMAGE_ROWS;

            if needs_downscale || can_expand {
                if let Some(flag) = self.pending_media.remove(&key) {
                    flag.store(true, Ordering::SeqCst);
                }
                self.remove_pending_media_tracking(&key);
                self.media_previews.remove(&key);
                self.media_failures.remove(&key);
                self.queue_active_kitty_delete();
                self.request_media_preview_with_limits(&post.post, max_cols, max_rows, true);

                if !lines.is_empty() && !line_is_blank(lines.last().unwrap()) {
                    lines.push(Line::raw(String::new()));
                }
                let offset = lines.len();
                lines.push(Line::from(Span::styled(
                    "Loading preview...",
                    Style::default().fg(COLOR_TEXT_SECONDARY),
                )));
                self.media_layouts.insert(
                    key.clone(),
                    MediaLayout {
                        line_offset: offset,
                        indent: MEDIA_INDENT,
                    },
                );
            } else {
                if !lines.is_empty() && !line_is_blank(lines.last().unwrap()) {
                    lines.push(Line::raw(String::new()));
                }
                let offset = lines.len();
                lines.extend(preview.placeholder().lines.clone());
                self.media_layouts.insert(
                    key.clone(),
                    MediaLayout {
                        line_offset: offset,
                        indent: MEDIA_INDENT,
                    },
                );
                if preview.has_kitty() {
                    self.needs_kitty_flush = true;
                }
                if preview.video().is_some() && self.active_video.is_some() {
                    self.needs_video_refresh = true;
                }
            }
        } else if !self.media_failures.contains(&key) {
            let mut offset = lines.len();
            if !lines.is_empty() && !line_is_blank(lines.last().unwrap()) {
                offset = lines.len();
                lines.push(Line::raw(String::new()));
            }
            lines.push(Line::from(Span::styled(
                "Loading preview...",
                Style::default().fg(COLOR_TEXT_SECONDARY),
            )));
            self.media_layouts.insert(
                key.clone(),
                MediaLayout {
                    line_offset: offset,
                    indent: MEDIA_INDENT,
                },
            );
        }
        Text {
            lines,
            alignment: base.alignment,
            style: base.style,
        }
    }

    fn compose_fullscreen_preview(&mut self, post: &PostPreview) -> Text<'static> {
        let key = post.post.name.clone();
        self.media_layouts.remove(&key);

        if !self.kitty_status.is_enabled() {
            return Text::from(vec![
                Line::from(Span::styled(
                    "Inline previews are disabled in this terminal.",
                    Style::default().fg(COLOR_TEXT_SECONDARY),
                )),
                Line::default(),
                self.fullscreen_hint_line(),
            ]);
        }

        let (max_cols, max_rows) = self.media_constraints();

        let available_cols = self.media_constraints.cols.max(1);
        let available_rows = self.media_constraints.rows.max(1);

        if let Some(preview) = self.media_previews.get(&key).cloned() {
            if preview.video().is_some() {
                let _ = self.stop_active_video(None, true);
                return Text::from(vec![
                    Line::from(Span::styled(
                        "Video playback runs in the inline view. Press f to return.",
                        Style::default().fg(COLOR_TEXT_SECONDARY),
                    )),
                    Line::default(),
                    self.fullscreen_hint_line(),
                ]);
            }
            let (cols, rows) = preview.dims();
            let expand_cols = preview.limited_cols() && max_cols > cols;
            let expand_rows = preview.limited_rows() && max_rows > rows;

            if expand_cols || expand_rows {
                if let Some(cancel) = self.pending_media.remove(&key) {
                    cancel.store(true, Ordering::SeqCst);
                }
                self.remove_pending_media_tracking(&key);
                self.media_previews.remove(&key);
                self.media_failures.remove(&key);
                self.request_media_preview_with_limits(&post.post, max_cols, max_rows, true);
                self.media_layouts.insert(
                    key,
                    MediaLayout {
                        line_offset: 0,
                        indent: 0,
                    },
                );
                return Text::from(vec![Line::from(Span::styled(
                    "Loading preview...",
                    Style::default().fg(COLOR_TEXT_SECONDARY),
                ))]);
            }

            let indent = ((available_cols - cols).max(0) / 2) as u16;
            let top_padding = ((available_rows - rows).max(0) / 2) as usize;

            let mut lines = Vec::with_capacity(top_padding + preview.placeholder().lines.len() + 3);
            lines.resize_with(top_padding, Line::default);
            lines.extend(preview.placeholder().lines.clone());
            self.media_layouts.insert(
                key,
                MediaLayout {
                    line_offset: top_padding,
                    indent,
                },
            );
            if preview.has_kitty() {
                self.needs_kitty_flush = true;
            }
            if preview.limited_cols() || preview.limited_rows() {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "Preview scaled to fit current viewport.",
                    Style::default().fg(COLOR_TEXT_SECONDARY),
                )));
            }
            lines.push(Line::default());
            lines.push(self.fullscreen_hint_line());
            return Text::from(lines);
        }

        if self.media_failures.contains(&key) {
            return Text::from(vec![
                Line::from(Span::styled(
                    "Failed to load preview.",
                    Style::default().fg(COLOR_ERROR),
                )),
                Line::default(),
                self.fullscreen_hint_line(),
            ]);
        }

        if !self.pending_media.contains_key(&key) {
            self.request_media_preview_with_limits(&post.post, max_cols, max_rows, true);
        }
        self.media_layouts.insert(
            key,
            MediaLayout {
                line_offset: 0,
                indent: 0,
            },
        );
        Text::from(vec![
            Line::from(Span::styled(
                "Loading preview...",
                Style::default().fg(COLOR_TEXT_SECONDARY),
            )),
            Line::default(),
            self.fullscreen_hint_line(),
        ])
    }

    fn fullscreen_hint_line(&self) -> Line<'static> {
        Line::from(Span::styled(
            "Press f to return · j/k scroll",
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .add_modifier(Modifier::ITALIC),
        ))
    }

    fn refresh_selected_post_media(&mut self) {
        let Some(post) = self.posts.get(self.selected_post).cloned() else {
            return;
        };
        let key = post.post.name.clone();
        if self.media_fullscreen {
            self.content = self.compose_fullscreen_preview(&post);
            return;
        }
        if let Some(rendered) = self.content_cache.get(&key).cloned() {
            let scroll = self.content_scroll;
            self.content = self.compose_content(rendered, &post);
            let max_scroll = self
                .content
                .lines
                .len()
                .saturating_sub(1)
                .min(u16::MAX as usize);
            self.content_scroll = scroll.min(max_scroll as u16);
        }
        self.ensure_media_request_ready(&post);
    }

    fn queue_content_render(&mut self, post_name: String, source: String) {
        if let Some(pending) = self.pending_content.take() {
            pending.cancel_flag.store(true, Ordering::SeqCst);
        }

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.pending_content = Some(PendingContent {
            request_id,
            post_name: post_name.clone(),
            cancel_flag: cancel_flag.clone(),
        });

        let tx = self.response_tx.clone();
        thread::spawn(move || {
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let renderer = markdown::Renderer::new();
            let rendered = renderer.render(&source);
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(AsyncResponse::Content {
                request_id,
                post_name,
                rendered,
            });
        });
    }

    fn draw_content(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if self.media_fullscreen {
            frame.render_widget(Clear, area);
            let constraints_changed = self.update_media_constraints(area, true);
            self.content_area = Some(area);
            if constraints_changed {
                self.refresh_selected_post_media();
            }
            if self.selected_post_has_inline_media() {
                self.needs_kitty_flush = true;
            }
            let paragraph = Paragraph::new(self.content.clone())
                .style(Style::default().bg(COLOR_PANEL_BG).fg(COLOR_TEXT_PRIMARY))
                .wrap(Wrap { trim: false })
                .scroll((self.content_scroll, 0));
            frame.render_widget(paragraph, area);
        } else {
            let block = self.pane_block(Pane::Content);
            let inner = block.inner(area);
            let constraints_changed = self.update_media_constraints(inner, false);
            self.content_area = Some(inner);
            if constraints_changed {
                self.refresh_selected_post_media();
            }
            if self.selected_post_has_inline_media() {
                self.needs_kitty_flush = true;
            }
            let paragraph = Paragraph::new(self.content.clone())
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((self.content_scroll, 0));
            frame.render_widget(paragraph, area);
        }
    }

    fn draw_comments(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = self.pane_block(Pane::Comments);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let focused = self.focused_pane == Pane::Comments;
        let sort_focused = focused && self.comment_sort_selected;
        let sort_lines = self.comment_sort_lines_for_width(inner.width as usize, sort_focused);
        let sort_height = sort_lines.len().max(1) as u16;

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(sort_height), Constraint::Min(1)])
            .split(inner);

        let sort_paragraph =
            Paragraph::new(sort_lines)
                .wrap(Wrap { trim: true })
                .style(Style::default().bg(if sort_focused {
                    COLOR_PANEL_SELECTED_BG
                } else {
                    COLOR_PANEL_BG
                }));
        frame.render_widget(sort_paragraph, layout[0]);

        let comment_area = layout[1];
        let width = comment_area.width.max(1) as usize;
        self.comment_view_height.set(comment_area.height);
        self.comment_view_width.set(comment_area.width);

        let total_visible = self.visible_comment_indices.len();
        let comment_status = if self.pending_comments.is_some() {
            format!("{} {}", self.spinner.frame(), self.comment_status)
                .trim()
                .to_string()
        } else {
            self.comment_status.clone()
        };
        let status_style = Style::default()
            .fg(COLOR_TEXT_SECONDARY)
            .bg(COLOR_PANEL_BG)
            .add_modifier(Modifier::BOLD);
        let mut status_lines = wrap_plain(&comment_status, width, status_style);
        status_lines.push(Line::from(Span::styled(String::new(), status_style)));
        pad_lines_to_width(&mut status_lines, comment_area.width);
        self.comment_status_height.set(status_lines.len());
        self.ensure_comment_visible();
        let offset = self.comment_offset.get().min(total_visible);
        let available_height = self.available_comment_height();
        let mut used_height = 0usize;
        let mut items: Vec<ListItem> =
            Vec::with_capacity(total_visible.saturating_sub(offset).saturating_add(1));
        items.push(ListItem::new(status_lines));
        for (visible_idx, comment_index) in
            self.visible_comment_indices.iter().enumerate().skip(offset)
        {
            let comment = match self.comments.get(*comment_index) {
                Some(entry) => entry,
                None => continue,
            };
            let selected = visible_idx == self.selected_comment && !self.comment_sort_selected;
            let highlight = focused && selected;
            let background = if highlight {
                COLOR_PANEL_SELECTED_BG
            } else {
                COLOR_PANEL_BG
            };

            let mut meta_style = Style::default()
                .fg(comment_depth_color(comment.depth))
                .bg(background);
            if highlight {
                meta_style = meta_style.add_modifier(Modifier::BOLD);
            } else if focused {
                meta_style = meta_style.add_modifier(Modifier::ITALIC);
            }

            let body_color = if highlight || focused || selected {
                COLOR_TEXT_PRIMARY
            } else {
                COLOR_TEXT_SECONDARY
            };
            let body_style = Style::default().fg(body_color).bg(background);

            let collapsed = self.collapsed_comments.contains(comment_index);
            let indicator = if collapsed { "[+]" } else { "[-]" };

            let mut lines =
                comment_lines(comment, width, indicator, meta_style, body_style, collapsed);
            let item_height = lines.len().saturating_add(1);
            if available_height > 0
                && used_height > 0
                && used_height + item_height > available_height
            {
                break;
            }
            lines.push(Line::from(Span::styled(String::new(), body_style)));
            pad_lines_to_width(&mut lines, comment_area.width);
            items.push(ListItem::new(lines));
            if available_height == 0 {
                break;
            }
            if available_height > 0 {
                used_height = used_height.saturating_add(item_height.min(available_height));
                if used_height >= available_height {
                    break;
                }
            }
        }

        let list = List::new(items);
        frame.render_widget(list, comment_area);
    }

    fn draw_menu(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup_area = centered_rect(70, 70, area);
        frame.render_widget(Clear, popup_area);
        let menu = Paragraph::new(self.menu_body())
            .block(
                Block::default()
                    .title(Span::styled(
                        "Guided Menu",
                        Style::default()
                            .fg(COLOR_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(COLOR_ACCENT))
                    .style(Style::default().bg(COLOR_PANEL_BG)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(menu, popup_area);
    }

    fn menu_field_line(&self, field: MenuField) -> Line<'static> {
        let is_active = self.menu_form.active == field;
        let mut spans = Vec::new();
        let indicator_style = if is_active {
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_TEXT_SECONDARY)
        };
        spans.push(Span::styled(
            if is_active { ">" } else { " " }.to_string(),
            indicator_style,
        ));
        spans.push(Span::raw(" "));

        match field {
            MenuField::Save => {
                let button_style = if is_active {
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default()
                        .fg(COLOR_TEXT_SECONDARY)
                        .add_modifier(Modifier::BOLD)
                };
                spans.push(Span::styled("[ Save & Close ]".to_string(), button_style));
                spans.push(Span::raw("  Press Enter to write credentials"));
            }
            MenuField::OpenLink => {
                let button_style = if is_active {
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default()
                        .fg(COLOR_TEXT_SECONDARY)
                        .add_modifier(Modifier::BOLD)
                };
                let label = if self.menu_form.auth_pending {
                    "[ Open Link ]  Waiting for redirect… press Enter to open".to_string()
                } else {
                    "[ Open Link ]  Press Enter to open again".to_string()
                };
                spans.push(Span::styled(label, button_style));
            }
            _ => {
                let label_style = if is_active {
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(COLOR_TEXT_SECONDARY)
                        .add_modifier(Modifier::BOLD)
                };
                spans.push(Span::styled(field.title().to_string(), label_style));
                spans.push(Span::raw(": "));

                let display = self.menu_form.display_value(field);
                let value_style = if display == "(not set)" {
                    Style::default().fg(COLOR_TEXT_SECONDARY)
                } else if is_active {
                    Style::default().fg(COLOR_ACCENT)
                } else {
                    Style::default().fg(COLOR_TEXT_PRIMARY)
                };
                spans.push(Span::styled(display, value_style));
            }
        }

        Line::from(spans)
    }

    fn menu_body(&self) -> Text<'static> {
        match self.menu_screen {
            MenuScreen::Accounts => self.menu_accounts_body(),
            MenuScreen::Credentials => self.menu_credentials_body(),
            MenuScreen::ReleaseNotes => self.menu_release_notes_body(),
        }
    }

    fn menu_accounts_body(&self) -> Text<'static> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            "Account Manager".to_string(),
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::default());

        if self.menu_accounts.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "No Reddit accounts saved.".to_string(),
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            )]));
        } else {
            for (idx, entry) in self.menu_accounts.iter().enumerate() {
                let selected = self.menu_account_index == idx;
                let indicator_style = Style::default().fg(if selected {
                    COLOR_ACCENT
                } else {
                    COLOR_TEXT_SECONDARY
                });
                let mut label_style = Style::default().fg(if selected {
                    COLOR_TEXT_PRIMARY
                } else {
                    COLOR_TEXT_SECONDARY
                });
                if selected {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                if entry.is_active {
                    label_style = label_style.add_modifier(Modifier::UNDERLINED);
                }
                let mut display = entry.display.clone();
                if entry.is_active {
                    display.push_str(" (active)");
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected { ">" } else { " " }.to_string(),
                        indicator_style,
                    ),
                    Span::raw(" "),
                    Span::styled(display, label_style),
                ]));
            }
        }

        let positions = self.menu_account_positions();

        let add_selected = self.menu_account_index == positions.add;
        let mut add_style = Style::default().fg(if add_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        if add_selected {
            add_style = add_style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(if add_selected { ">" } else { " " }.to_string(), add_style),
            Span::raw(" "),
            Span::styled("Add new account…".to_string(), add_style),
        ]));

        lines.push(Line::default());
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::styled(
            "Stay in the loop with the community:".to_string(),
            Style::default().fg(COLOR_TEXT_SECONDARY),
        )]));
        let join_index = positions.join;
        let join_selected = self.menu_account_index == join_index;
        let join_state = self.active_join_state();
        let label = if join_state.is_some_and(|state| state.pending) {
            "[ Joining r/ReddixTUI… ]"
        } else if join_state.is_some_and(|state| state.joined) {
            "[ Joined r/ReddixTUI ]"
        } else {
            "[ Join r/ReddixTUI ]"
        };
        let joined = join_state.is_some_and(|state| state.joined);
        let join_indicator_style = Style::default().fg(if join_selected {
            COLOR_ACCENT
        } else if joined {
            COLOR_SUCCESS
        } else {
            COLOR_TEXT_SECONDARY
        });
        let mut join_label_style = Style::default().fg(if joined {
            COLOR_SUCCESS
        } else if join_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        if join_selected && !joined {
            join_label_style = join_label_style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
        } else {
            join_label_style = join_label_style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(
                if join_selected { ">" } else { " " }.to_string(),
                join_indicator_style,
            ),
            Span::raw(" "),
            Span::styled(label.to_string(), join_label_style),
        ]));

        let (join_hint, join_hint_style) = match (join_state, self.active_account_id()) {
            (Some(state), _) if state.last_error.is_some() => (
                state.last_error.clone().unwrap(),
                Style::default().fg(COLOR_ERROR),
            ),
            (Some(state), _) if state.pending => (
                "Request sent… hang tight.".to_string(),
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            ),
            (Some(state), _) if state.joined => (
                "Already subscribed. Thanks for supporting the community!".to_string(),
                Style::default().fg(COLOR_SUCCESS),
            ),
            (_, Some(_)) => (
                "Press Enter to subscribe using your active account.".to_string(),
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            ),
            _ => (
                "Add an account to enable one-click subscribe.".to_string(),
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            ),
        };
        lines.push(Line::from(vec![Span::styled(join_hint, join_hint_style)]));
        if positions.release_notes.is_some() && self.release_note.is_some() {
            lines.push(Line::default());
        }
        if let Some(release_idx) = positions.release_notes {
            if let Some(note) = &self.release_note {
                let selected = self.menu_account_index == release_idx;
                let highlight_unread = self.release_note_unread && !selected;
                let indicator_style = Style::default().fg(if selected || highlight_unread {
                    COLOR_ACCENT
                } else {
                    COLOR_TEXT_SECONDARY
                });
                let mut label_style = Style::default().fg(if selected {
                    COLOR_TEXT_PRIMARY
                } else if highlight_unread {
                    COLOR_ACCENT
                } else {
                    COLOR_TEXT_SECONDARY
                });
                if selected || highlight_unread {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected { ">" } else { " " }.to_string(),
                        indicator_style,
                    ),
                    Span::raw(" "),
                    Span::styled(note.title.clone(), label_style),
                ]));
                let summary_style = Style::default()
                    .fg(if selected {
                        COLOR_TEXT_PRIMARY
                    } else if highlight_unread {
                        COLOR_ACCENT
                    } else {
                        COLOR_TEXT_SECONDARY
                    })
                    .add_modifier(Modifier::ITALIC);
                lines.push(Line::from(vec![Span::styled(
                    note.summary.clone(),
                    summary_style,
                )]));
            }
        }
        lines.push(Line::default());
        lines.push(Line::default());

        let update_index = positions.update_check;
        let update_selected = self.menu_account_index == update_index;
        let update_indicator_style = Style::default().fg(if update_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        let mut update_label_style = Style::default().fg(if update_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        if update_selected {
            update_label_style = update_label_style.add_modifier(Modifier::BOLD);
        }
        let has_update = self
            .latest_known_version
            .as_ref()
            .is_some_and(|latest| latest > &self.current_version);
        let summary_style = if has_update {
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC)
        } else {
            Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .add_modifier(Modifier::ITALIC)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if update_selected { ">" } else { " " }.to_string(),
                update_indicator_style,
            ),
            Span::raw(" "),
            Span::styled("Re-run update check · ".to_string(), update_label_style),
            Span::styled(format!("Reddix {}", self.version_summary()), summary_style),
        ]));

        if let Some(install_idx) = positions.install {
            let install_selected = self.menu_account_index == install_idx;
            let install_indicator_style = Style::default().fg(if install_selected {
                COLOR_ACCENT
            } else {
                COLOR_TEXT_SECONDARY
            });
            let mut install_label_style = Style::default().fg(if install_selected {
                COLOR_ACCENT
            } else {
                COLOR_TEXT_SECONDARY
            });
            if install_selected {
                install_label_style = install_label_style.add_modifier(Modifier::BOLD);
            }
            if self.update_install_in_progress {
                install_label_style = install_label_style.add_modifier(Modifier::ITALIC);
            }
            let install_text = if let Some(update) = &self.update_notice {
                if self.update_install_in_progress {
                    format!("Installing v{}…", update.version)
                } else {
                    format!("Install v{} now", update.version)
                }
            } else if self.update_install_in_progress {
                "Installing update…".to_string()
            } else {
                "Install latest update".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if install_selected { ">" } else { " " }.to_string(),
                    install_indicator_style,
                ),
                Span::raw(" "),
                Span::styled(install_text, install_label_style),
            ]));
            let hint_style = Style::default()
                .fg(COLOR_TEXT_SECONDARY)
                .add_modifier(Modifier::ITALIC);
            let hint_text = if self.update_install_in_progress {
                "Installer running in background…"
            } else {
                "Press Enter to download and run the official installer."
            };
            lines.push(Line::from(vec![Span::styled(
                hint_text.to_string(),
                hint_style,
            )]));
        }

        lines.push(Line::default());
        lines.push(Line::default());

        let github_index = positions.github;
        let support_index = positions.support;

        let github_selected = self.menu_account_index == github_index;
        let github_indicator_style = Style::default().fg(if github_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        let mut github_label_style = Style::default().fg(if github_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        if github_selected {
            github_label_style = github_label_style.add_modifier(Modifier::BOLD);
        }

        let support_selected = self.menu_account_index == support_index;
        let support_indicator_style = Style::default().fg(if support_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        let mut support_label_style = Style::default().fg(if support_selected {
            COLOR_ACCENT
        } else {
            COLOR_TEXT_SECONDARY
        });
        if support_selected {
            support_label_style = support_label_style.add_modifier(Modifier::BOLD);
        }

        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                if github_selected { ">" } else { " " }.to_string(),
                github_indicator_style,
            ),
            Span::raw(" "),
            Span::styled(
                "Check the project out on GitHub · ".to_string(),
                github_label_style,
            ),
            Span::styled(
                PROJECT_LINK_URL.to_string(),
                Style::default().fg(COLOR_ACCENT),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                if support_selected { ">" } else { " " }.to_string(),
                support_indicator_style,
            ),
            Span::raw(" "),
            Span::styled(
                "Support the project (opens browser) · ".to_string(),
                support_label_style,
            ),
            Span::styled(
                SUPPORT_LINK_URL.to_string(),
                Style::default().fg(COLOR_ACCENT),
            ),
        ]));
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::styled(
            "Controls: j/k select · Enter switch/select · a add account · Esc/m close".to_string(),
            Style::default().fg(COLOR_TEXT_SECONDARY),
        )]));

        Text::from(lines)
    }

    fn menu_credentials_body(&self) -> Text<'static> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            "Setup & Login Guide".to_string(),
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::raw(
            "1. Open Reddit app preferences at https://www.reddit.com/prefs/apps and create a script app."
                .to_string(),
        )]));
        lines.push(Line::from(vec![Span::raw(
            "2. Add the local redirect URI 127.0.0.1:65010/reddix/callback as authorized."
                .to_string(),
        )]));
        lines.push(Line::from(vec![Span::raw(format!(
            "3. Reddix will update {} with your credentials.",
            self.config_path
        ))]));
        lines.push(Line::from(vec![Span::raw(
            "4. After saving, press r in the main view to reload Reddit data.".to_string(),
        )]));
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::styled(
            "Credentials".to_string(),
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )]));
        let mut fields = vec![
            MenuField::ClientId,
            MenuField::ClientSecret,
            MenuField::UserAgent,
            MenuField::Save,
        ];
        if self.menu_form.has_auth_link() {
            fields.push(MenuField::OpenLink);
        }
        for field in fields {
            lines.push(self.menu_field_line(field));
        }
        if self.menu_form.auth_url.is_some() {
            lines.push(Line::default());
            lines.push(Line::from(vec![Span::styled(
                "Authorization Link".to_string(),
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            )]));
            let message = if self.menu_form.auth_pending {
                "Link ready (press Enter to open)".to_string()
            } else {
                "Press Enter to open the authorization link again".to_string()
            };
            lines.push(Line::from(vec![Span::styled(
                message,
                Style::default().fg(COLOR_ACCENT),
            )]));
            if self.menu_form.auth_pending {
                lines.push(Line::from(vec![Span::raw(
                    "Waiting for Reddit to redirect back to Reddix...".to_string(),
                )]));
            }
        }
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::raw(
            "Controls: Tab/Shift-Tab or Up/Down to move | Backspace/Delete to edit | Enter to advance/save/open | Esc back | m close"
                .to_string(),
        )]));
        if let Some(status) = &self.menu_form.status {
            lines.push(Line::default());
            let lowered = status.to_lowercase();
            let style = if lowered.contains("fail") || lowered.contains("error") {
                Style::default().fg(COLOR_ERROR)
            } else {
                Style::default().fg(COLOR_SUCCESS)
            };
            lines.push(Line::from(vec![Span::styled(status.clone(), style)]));
        }
        Text::from(lines)
    }

    fn menu_release_notes_body(&self) -> Text<'static> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(note) = &self.release_note {
            lines.push(Line::from(vec![Span::styled(
                note.title.clone(),
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::default());
            lines.push(Line::from(vec![Span::styled(
                format!("Version {}", note.version),
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            )]));
            lines.push(Line::default());
            for detail in &note.details {
                lines.push(Line::from(vec![
                    Span::styled("- ".to_string(), Style::default().fg(COLOR_ACCENT)),
                    Span::styled(detail.clone(), Style::default().fg(COLOR_TEXT_PRIMARY)),
                ]));
            }
            lines.push(Line::default());
            lines.push(Line::from(vec![Span::styled(
                "Press Enter or o to open the full release notes in your browser.",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "Press Esc to return to the account list.",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                "No release notes available right now.",
                Style::default()
                    .fg(COLOR_TEXT_SECONDARY)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }
        Text::from(lines)
    }

    fn footer_text(&self) -> String {
        if self.menu_visible {
            return match self.menu_screen {
                MenuScreen::Accounts => {
                    "Guided menu: j/k select account · Enter switch · a add account · Esc/m close"
                        .to_string()
                }
                MenuScreen::Credentials => {
                    "Guided menu: Tab/Shift-Tab change field · Enter save/advance/open · Esc back · m close"
                        .to_string()
                }
                MenuScreen::ReleaseNotes => {
                    "Guided menu: Enter/o open release page · Esc back · m close".to_string()
                }
            };
        }

        if self.help_visible {
            return "Help: Esc or ? to close".to_string();
        }

        if self.comment_composer.is_some() {
            return "Comment composer: type to edit · Ctrl+S submit · Esc cancel".to_string();
        }

        if self.action_menu_visible {
            return "Actions: j/k move · Enter/l open · h/Esc close".to_string();
        }

        if self.media_fullscreen {
            return "Fullscreen preview: f return · j/k scroll".to_string();
        }

        let mut parts: Vec<String> = Vec::new();

        match self.focused_pane {
            Pane::Navigation => match self.nav_mode {
                NavMode::Sorts => {
                    parts.push("Navigation: ←/→ sort · Enter load".to_string());
                }
                NavMode::Subreddits => {
                    parts.push("Subreddits: j/k move · Enter load".to_string());
                }
            },
            Pane::Posts => {
                if self.posts.is_empty() {
                    parts.push("Posts: waiting for feed…".to_string());
                } else {
                    parts.push("Posts: j/k move · Enter open · h go to navigation".to_string());
                }
            }
            Pane::Content => {
                parts.push("Content: ↑/↓ scroll".to_string());
                if self.can_toggle_fullscreen_preview() {
                    parts.push("f fullscreen preview".to_string());
                }
            }
            Pane::Comments => {
                if self.pending_comments.is_some() {
                    parts.push("Loading comments…".to_string());
                } else if !self.comments.iter().any(|entry| !entry.is_post_root) {
                    parts.push("No comments yet".to_string());
                } else {
                    parts.push("Comments: j/k move · ←/→ sort · y copy · w reply".to_string());
                }
            }
        }

        if self.pending_posts.is_some() {
            parts.push("Refreshing feed…".to_string());
        }

        if self.active_video.is_some() {
            parts.push("Video: q stop playback".to_string());
        }

        parts.push("h/l focus panes".to_string());
        parts.push("o actions menu".to_string());
        parts.push("w write comment".to_string());
        parts.push("? help".to_string());
        parts.push("m guided menu".to_string());
        parts.push("q quit".to_string());

        parts.join(" · ")
    }
}

fn resolve_current_version() -> Version {
    let base = || Version::parse(crate::VERSION).expect("crate version is valid semver");

    match env::var(CURRENT_VERSION_OVERRIDE_ENV) {
        Ok(raw) => {
            let candidate = raw.trim();
            if candidate.is_empty() {
                return base();
            }
            match Version::parse(candidate) {
                Ok(version) => version,
                Err(err) => {
                    eprintln!(
                        "Ignoring {}='{}': parse error {}",
                        CURRENT_VERSION_OVERRIDE_ENV, raw, err
                    );
                    base()
                }
            }
        }
        Err(env::VarError::NotPresent) => base(),
        Err(err) => {
            eprintln!(
                "Ignoring {} (failed to read env var): {}",
                CURRENT_VERSION_OVERRIDE_ENV, err
            );
            base()
        }
    }
}

fn pane_constraints(panes: &[Pane; 3]) -> [Constraint; 3] {
    match panes {
        [Pane::Navigation, Pane::Posts, Pane::Content] => [
            Constraint::Percentage(20),
            Constraint::Percentage(45),
            Constraint::Percentage(35),
        ],
        [Pane::Posts, Pane::Content, Pane::Comments] => [
            Constraint::Percentage(30),
            Constraint::Percentage(25),
            Constraint::Percentage(45),
        ],
        [Pane::Navigation, Pane::Posts, Pane::Comments] => [
            Constraint::Percentage(20),
            Constraint::Percentage(35),
            Constraint::Percentage(45),
        ],
        [Pane::Navigation, Pane::Content, Pane::Comments] => [
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(50),
        ],
        _ => [
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Percentage(40),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn total_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn resolve_current_version_respects_override() {
        let _guard = env_guard();
        env::set_var(CURRENT_VERSION_OVERRIDE_ENV, "1.2.3");
        let version = resolve_current_version();
        env::remove_var(CURRENT_VERSION_OVERRIDE_ENV);
        assert_eq!(version, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn resolve_current_version_falls_back_on_invalid_override() {
        let _guard = env_guard();
        env::set_var(CURRENT_VERSION_OVERRIDE_ENV, "not-a-version");
        let version = resolve_current_version();
        env::remove_var(CURRENT_VERSION_OVERRIDE_ENV);
        assert_eq!(version, Version::parse(crate::VERSION).unwrap());
    }

    #[test]
    fn pad_lines_extends_to_width() {
        let mut lines = vec![Line::from(vec![Span::raw("abc")])];
        pad_lines_to_width(&mut lines, 6);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[1].content.as_ref(), "   ");
        assert_eq!(total_width(&lines[0]), 6);
    }

    #[test]
    fn pad_lines_does_not_shorten() {
        let mut lines = vec![Line::from(vec![Span::raw("abcdef")])];
        pad_lines_to_width(&mut lines, 4);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(total_width(&lines[0]), 6);
    }

    #[test]
    fn pad_lines_supports_wide_glyphs() {
        let mut lines = vec![Line::from(vec![Span::raw("🦀")])];
        pad_lines_to_width(&mut lines, 3);
        assert_eq!(total_width(&lines[0]), 3);
        assert_eq!(lines[0].spans.len(), 2);
    }

    #[test]
    fn indent_media_preview_unchanged_when_indent_zero() {
        let preview = "line one\nline two";
        assert_eq!(indent_media_preview(preview), preview);
    }

    #[test]
    fn kitty_placeholder_matches_dimensions() {
        let placeholder = kitty_placeholder_text(4, 2, 0, "example");
        assert_eq!(placeholder.lines.len(), 3);
        assert_eq!(placeholder.lines[0].spans[0].content.as_ref(), "    ");
        assert_eq!(
            placeholder.lines[2].spans[0].content.as_ref(),
            "[image: example]"
        );
    }
}
