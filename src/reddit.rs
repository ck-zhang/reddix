use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE, COOKIE, USER_AGENT};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use std::sync::RwLock;
use url::Url;

pub const DEFAULT_BASE_URL: &str = "https://oauth.reddit.com/";

pub trait TokenProvider: Send + Sync {
    fn token(&self) -> Result<OAuthToken>;
}

#[derive(Debug, Clone)]
pub struct OAuthToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Default)]
pub struct ClientConfig {
    pub user_agent: String,
    pub base_url: Option<String>,
    pub http_client: Option<HttpClient>,
    pub cookie_header: Option<String>,
    pub bearer_auth: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ListingOptions {
    pub after: Option<String>,
    pub before: Option<String>,
    pub limit: Option<u32>,
    pub extra: Vec<(String, String)>,
}

impl ListingOptions {
    fn into_params(self) -> Vec<(String, String)> {
        let mut params = Vec::new();
        if let Some(after) = self.after {
            params.push(("after".into(), after));
        }
        if let Some(before) = self.before {
            params.push(("before".into(), before));
        }
        if let Some(limit) = self.limit {
            params.push(("limit".into(), limit.to_string()));
        }
        params.extend(self.extra);
        params
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortOption {
    #[default]
    Hot,
    New,
    Top,
    Best,
    Rising,
}

impl SortOption {
    fn as_str(&self) -> &'static str {
        match self {
            SortOption::Hot => "hot",
            SortOption::New => "new",
            SortOption::Top => "top",
            SortOption::Best => "best",
            SortOption::Rising => "rising",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum CommentSortOption {
    #[default]
    Confidence,
    Top,
    New,
    Controversial,
    Old,
    Qa,
}

impl CommentSortOption {
    fn as_str(&self) -> &'static str {
        match self {
            CommentSortOption::Confidence => "confidence",
            CommentSortOption::Top => "top",
            CommentSortOption::New => "new",
            CommentSortOption::Controversial => "controversial",
            CommentSortOption::Old => "old",
            CommentSortOption::Qa => "qa",
        }
    }
}

fn sanitize_username(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("reddit: username required");
    }
    let without_slashes = trimmed.trim_start_matches('/');
    let rest = without_slashes
        .strip_prefix("u/")
        .or_else(|| without_slashes.strip_prefix("U/"))
        .or_else(|| without_slashes.strip_prefix("user/"))
        .unwrap_or(without_slashes)
        .trim_start_matches('/');
    let normalized = rest.trim();
    if normalized.is_empty() {
        bail!("reddit: username required");
    }
    Ok(normalized.to_string())
}

pub struct Client {
    token_provider: Arc<dyn TokenProvider>,
    http: HttpClient,
    user_agent: String,
    base_url: Url,
    cookie_header: Option<String>,
    bearer_auth: bool,
    rate: RwLock<RateLimit>,
}

#[derive(Debug, Clone, Default)]
pub struct RateLimit {
    pub used: f64,
    pub remaining: f64,
    pub reset_at: Option<SystemTime>,
}

impl Client {
    pub fn new(token_provider: Arc<dyn TokenProvider>, config: ClientConfig) -> Result<Self> {
        if config.user_agent.trim().is_empty() {
            bail!("reddit client user agent required");
        }
        let base = config
            .base_url
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let base_url = Url::parse(&base)?;
        let http = match config.http_client {
            Some(client) => client,
            None => HttpClient::builder()
                .timeout(Duration::from_secs(20))
                .build()?,
        };

        Ok(Client {
            token_provider,
            http,
            user_agent: config.user_agent,
            base_url,
            cookie_header: config.cookie_header,
            bearer_auth: config.bearer_auth,
            rate: RwLock::new(RateLimit::default()),
        })
    }

    pub fn rate_limit(&self) -> RateLimit {
        self.rate.read().unwrap().clone()
    }

    pub fn subreddit_listing(
        &self,
        subreddit: &str,
        sort: SortOption,
        opts: ListingOptions,
    ) -> Result<Listing<Post>> {
        let path = if subreddit.is_empty() {
            format!("/{}.json", sort.as_str())
        } else {
            format!(
                "/r/{}/{}.json",
                subreddit.trim_start_matches("r/"),
                sort.as_str()
            )
        };
        self.fetch_listing(&path, opts)
    }

    pub fn front_page(&self, sort: SortOption, opts: ListingOptions) -> Result<Listing<Post>> {
        self.subreddit_listing("", sort, opts)
    }

    pub fn user_listing(
        &self,
        username: &str,
        sort: SortOption,
        mut opts: ListingOptions,
    ) -> Result<Listing<Post>> {
        let normalized = sanitize_username(username)?;
        let path = format!("/user/{}/submitted.json", normalized);
        opts.extra.push(("sort".into(), sort.as_str().to_string()));
        self.fetch_listing(&path, opts)
    }

    pub fn search_posts(
        &self,
        query: &str,
        sort: SortOption,
        mut opts: ListingOptions,
    ) -> Result<Listing<Post>> {
        let cleaned = query.trim();
        if cleaned.is_empty() {
            bail!("reddit: search query required");
        }
        opts.extra.push(("q".into(), cleaned.to_string()));
        opts.extra.push(("sort".into(), sort.as_str().to_string()));
        opts.extra.push(("type".into(), "link".into()));
        self.fetch_listing("/search.json", opts)
    }

    pub fn comments(
        &self,
        subreddit: &str,
        article: &str,
        sort: CommentSortOption,
        opts: ListingOptions,
    ) -> Result<PostComments> {
        let base = subreddit.trim_start_matches("r/");
        let path = if base.is_empty() {
            format!("/comments/{}.json", article)
        } else {
            format!("/r/{}/comments/{}.json", base, article)
        };
        let mut params = opts.into_params();
        params.push(("sort".into(), sort.as_str().to_string()));
        let payload: Vec<Value> = self.get_json(&path, &params)?;
        if payload.len() < 2 {
            bail!("reddit: comments payload missing elements");
        }
        let post_listing: ListingEnvelope<Post> =
            serde_json::from_value(payload[0].clone()).context("reddit: decode post listing")?;
        let comments_listing: ListingEnvelope<Comment> =
            serde_json::from_value(payload[1].clone()).context("reddit: decode comment listing")?;
        let post = post_listing
            .data
            .children
            .into_iter()
            .next()
            .map(|thing| thing.data)
            .ok_or_else(|| anyhow!("reddit: post listing empty"))?;
        Ok(PostComments {
            post,
            comments: comments_listing.data,
        })
    }

    pub fn subreddits(
        &self,
        source: SubredditSource,
        opts: ListingOptions,
    ) -> Result<Listing<Subreddit>> {
        let path = format!("{}.json", source.as_path());
        self.fetch_listing(&path, opts)
    }

    pub fn vote(&self, fullname: &str, dir: i32) -> Result<()> {
        if !(-1..=1).contains(&dir) {
            bail!("reddit: vote direction must be -1, 0, or 1");
        }
        let form = vec![
            ("id".to_string(), fullname.to_string()),
            ("dir".to_string(), dir.to_string()),
        ];
        self.request(Method::POST, "/api/vote", &[], Some(form))?;
        Ok(())
    }

    pub fn save(&self, fullname: &str, category: Option<&str>) -> Result<()> {
        let mut form = vec![("id".to_string(), fullname.to_string())];
        if let Some(cat) = category {
            if !cat.is_empty() {
                form.push(("category".into(), cat.into()));
            }
        }
        self.request(Method::POST, "/api/save", &[], Some(form))?;
        Ok(())
    }

    pub fn unsave(&self, fullname: &str) -> Result<()> {
        let form = vec![("id".to_string(), fullname.to_string())];
        self.request(Method::POST, "/api/unsave", &[], Some(form))?;
        Ok(())
    }

    pub fn hide(&self, fullname: &str) -> Result<()> {
        let form = vec![("id".to_string(), fullname.to_string())];
        self.request(Method::POST, "/api/hide", &[], Some(form))?;
        Ok(())
    }

    pub fn unhide(&self, fullname: &str) -> Result<()> {
        let form = vec![("id".to_string(), fullname.to_string())];
        self.request(Method::POST, "/api/unhide", &[], Some(form))?;
        Ok(())
    }

    pub fn subscribe_subreddit(&self, subreddit: &str) -> Result<()> {
        let name = subreddit.trim().trim_start_matches("r/");
        if name.is_empty() {
            bail!("reddit: subreddit name required");
        }
        let form = vec![
            ("action".to_string(), "sub".to_string()),
            ("sr_name".to_string(), name.to_string()),
        ];
        self.request(Method::POST, "/api/subscribe", &[], Some(form))?;
        Ok(())
    }

    pub fn is_subscribed(&self, subreddit: &str) -> Result<bool> {
        let name = subreddit.trim().trim_start_matches("r/");
        if name.is_empty() {
            bail!("reddit: subreddit name required");
        }
        let path = format!("/r/{}/about.json", name);
        let about: SubredditAboutEnvelope = self.get_json(&path, &[])?;
        Ok(about.data.user_is_subscriber)
    }

    pub fn reply(&self, parent: &str, text: &str) -> Result<Comment> {
        if parent.trim().is_empty() {
            bail!("reddit: reply parent is required");
        }
        if text.trim().is_empty() {
            bail!("reddit: reply text is required");
        }
        let form = vec![
            ("parent".to_string(), parent.to_string()),
            ("text".to_string(), text.to_string()),
            ("api_type".to_string(), "json".to_string()),
        ];
        let resp = self.request(Method::POST, "/api/comment", &[], Some(form))?;
        let payload: CommentResponse = resp.json()?;
        if let Some(err) = payload.json.errors.first() {
            let joined = err
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("reddit: comment error: {}", joined);
        }
        let comment = payload
            .json
            .data
            .things
            .into_iter()
            .next()
            .map(|thing| thing.data)
            .ok_or_else(|| anyhow!("reddit: comment response empty"))?;
        Ok(comment)
    }

    fn fetch_listing<T>(&self, path: &str, opts: ListingOptions) -> Result<Listing<T>>
    where
        T: DeserializeOwned,
    {
        let params = opts.into_params();
        let listing: ListingEnvelope<T> = self.get_json(path, &params)?;
        Ok(listing.data)
    }

    fn get_json<T>(&self, path: &str, params: &[(String, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        if self.cookie_header.is_none() {
            let resp = self.request(Method::GET, path, params, None)?;
            return Ok(resp.json()?);
        }

        let mut url = self.base_url.join(path)?;
        if !params.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }

        let cookie_header = self.cookie_header.as_ref().expect("checked above");
        let output = Command::new("curl")
            .arg("-sS")
            .arg("-L")
            .arg("-A")
            .arg(&self.user_agent)
            .arg("-H")
            .arg(format!("Cookie: {cookie_header}"))
            .arg("-H")
            .arg("accept: application/json,text/plain,*/*")
            .arg("-H")
            .arg("accept-language: en-US,en;q=0.9")
            .arg(url.as_str())
            .output()
            .context("reddit: run curl transport")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("reddit: curl transport failed: {}", stderr.trim());
        }

        serde_json::from_slice(&output.stdout).context("reddit: decode curl transport response")
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        params: &[(String, String)],
        form: Option<Vec<(String, String)>>,
    ) -> Result<Response> {
        let token = self.token_provider.token()?;
        let mut url = self.base_url.join(path)?;
        if !params.is_empty() {
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in params {
                    pairs.append_pair(k, v);
                }
            }
        }

        let mut req = self.http.request(method, url);
        req = req.header(USER_AGENT, self.user_agent.clone());
        if self.cookie_header.is_some() {
            req = req
                .header("accept", "application/json,text/plain,*/*")
                .header("accept-language", "en-US,en;q=0.9")
                .header("referer", "https://www.reddit.com/");
        }
        if self.bearer_auth {
            let auth_value = format!("Bearer {}", token.access_token);
            req = req.header(AUTHORIZATION, auth_value);
        }
        if let Some(cookie_header) = &self.cookie_header {
            req = req.header(COOKIE, cookie_header.clone());
        }
        if let Some(form_data) = form {
            req = req.header(CONTENT_TYPE, "application/x-www-form-urlencoded");
            req = req.form(&form_data);
        }

        let resp = req.send()?;
        self.capture_rate(resp.headers());
        if resp.status().is_success() {
            Ok(resp)
        } else {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            match status.as_u16() {
                401 => Err(anyhow!("reddit: unauthorized")),
                403 => Err(anyhow!("reddit: forbidden")),
                429 => Err(anyhow!("reddit: rate limited: {}", body)),
                _ => Err(anyhow!("reddit: api error {}: {}", status, body)),
            }
        }
    }

    fn capture_rate(&self, headers: &HeaderMap) {
        let remaining = header_float(headers, "x-ratelimit-remaining");
        let used = header_float(headers, "x-ratelimit-used");
        let reset = header_float(headers, "x-ratelimit-reset");
        if remaining == 0.0 && used == 0.0 && reset == 0.0 {
            return;
        }
        let reset_at = SystemTime::now().checked_add(Duration::from_secs_f64(reset.max(0.0)));
        let mut rate = self.rate.write().unwrap();
        rate.remaining = remaining;
        rate.used = used;
        rate.reset_at = reset_at;
    }
}

fn header_float(headers: &HeaderMap, key: &str) -> f64 {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listing<T> {
    pub after: Option<String>,
    pub before: Option<String>,
    pub children: Vec<Thing<T>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thing<T> {
    pub kind: String,
    pub data: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub name: String,
    pub title: String,
    pub subreddit: String,
    pub author: String,
    #[serde(default)]
    pub selftext: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub permalink: String,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub likes: Option<bool>,
    #[serde(default)]
    pub num_comments: i64,
    #[serde(default)]
    pub created_utc: f64,
    #[serde(default)]
    pub thumbnail: String,
    #[serde(default)]
    pub stickied: bool,
    #[serde(default)]
    pub over_18: bool,
    #[serde(default)]
    pub spoiler: bool,
    #[serde(default)]
    pub post_hint: String,
    #[serde(default)]
    pub is_video: bool,
    #[serde(default)]
    pub media: Option<PostMedia>,
    #[serde(default, rename = "secure_media")]
    pub secure_media: Option<PostMedia>,
    #[serde(default, rename = "crosspost_parent_list")]
    pub crosspost_parent_list: Vec<CrosspostParent>,
    #[serde(default)]
    pub preview: Preview,
    #[serde(default)]
    pub gallery_data: Option<GalleryData>,
    #[serde(default)]
    pub media_metadata: Option<std::collections::HashMap<String, MediaMetadata>>,
}

impl Post {
    pub fn created_at(&self) -> Option<SystemTime> {
        if self.created_utc == 0.0 {
            return None;
        }
        let secs = self.created_utc.trunc() as u64;
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Preview {
    #[serde(default)]
    pub images: Vec<PreviewImage>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreviewImage {
    pub source: PreviewSource,
    #[serde(default)]
    pub resolutions: Vec<PreviewSource>,
    #[serde(default)]
    pub variants: std::collections::HashMap<String, PreviewVariant>,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreviewSource {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreviewVariant {
    pub source: PreviewSource,
    #[serde(default)]
    pub resolutions: Vec<PreviewSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostMedia {
    #[serde(default, rename = "reddit_video")]
    pub reddit_video: Option<RedditVideo>,
    #[serde(default)]
    pub oembed: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RedditVideo {
    #[serde(default)]
    pub fallback_url: String,
    #[serde(default)]
    pub dash_url: String,
    #[serde(default)]
    pub hls_url: String,
    #[serde(default)]
    pub scrubber_media_url: String,
    #[serde(default)]
    pub duration: i64,
    #[serde(default)]
    pub is_gif: bool,
    #[serde(default)]
    pub height: i64,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub transcoding_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrosspostParent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub post_hint: String,
    #[serde(default)]
    pub media: Option<PostMedia>,
    #[serde(default, rename = "secure_media")]
    pub secure_media: Option<PostMedia>,
    #[serde(default)]
    pub preview: Preview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryData {
    pub items: Vec<GalleryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryItem {
    pub id: i64,
    #[serde(rename = "media_id")]
    pub media_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaMetadata {
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "e")]
    pub kind: String,
    #[serde(default, rename = "m")]
    pub mime: String,
    #[serde(default, rename = "s")]
    pub full: MediaMetadataImage,
    #[serde(default, rename = "p")]
    pub preview: Vec<MediaMetadataImage>,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaMetadataImage {
    #[serde(default, rename = "u")]
    pub url: String,
    #[serde(default, rename = "x")]
    pub width: i64,
    #[serde(default, rename = "y")]
    pub height: i64,
    #[serde(default)]
    pub gif: Option<String>,
    #[serde(default)]
    pub mp4: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comment {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub likes: Option<bool>,
    #[serde(default)]
    pub score_hidden: bool,
    #[serde(default)]
    pub depth: i64,
    #[serde(default)]
    pub created_utc: f64,
    #[serde(default)]
    pub replies: Option<Box<Listing<Comment>>>,
}

impl<'de> Deserialize<'de> for Comment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct CommentHelper {
            id: String,
            name: String,
            #[serde(default)]
            body: String,
            #[serde(default)]
            author: String,
            #[serde(default)]
            score: i64,
            #[serde(default)]
            likes: Option<bool>,
            #[serde(default)]
            score_hidden: bool,
            #[serde(default)]
            depth: i64,
            #[serde(default)]
            created_utc: f64,
            #[serde(default)]
            replies: serde_json::Value,
        }

        let helper = CommentHelper::deserialize(deserializer)?;
        let replies = if helper.replies.is_null() || helper.replies == "" {
            None
        } else {
            serde_json::from_value::<ListingEnvelope<Comment>>(helper.replies)
                .ok()
                .map(|listing| Box::new(listing.data))
        };
        Ok(Comment {
            id: helper.id,
            name: helper.name,
            body: helper.body,
            author: helper.author,
            score: helper.score,
            likes: helper.likes,
            score_hidden: helper.score_hidden,
            depth: helper.depth,
            created_utc: helper.created_utc,
            replies,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostComments {
    pub post: Post,
    pub comments: Listing<Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subreddit {
    pub id: String,
    #[serde(rename = "display_name_prefixed")]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subscribers: i64,
    #[serde(default, rename = "over18")]
    pub over_18: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubredditAboutEnvelope {
    data: SubredditAbout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubredditAbout {
    #[serde(default)]
    pub user_is_subscriber: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum SubredditSource {
    Subscriptions,
    Popular,
    Trending,
}

impl SubredditSource {
    fn as_path(&self) -> &'static str {
        match self {
            SubredditSource::Subscriptions => "/subreddits/mine/subscriber",
            SubredditSource::Popular => "/subreddits/popular",
            SubredditSource::Trending => "/subreddits/trending",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListingEnvelope<T> {
    kind: String,
    data: Listing<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommentResponse {
    json: CommentResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommentResponseBody {
    errors: Vec<Vec<serde_json::Value>>,
    data: CommentResponseData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommentResponseData {
    things: Vec<Thing<Comment>>,
}
