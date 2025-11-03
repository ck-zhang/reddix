use std::sync::Arc;

use anyhow::{Context, Result};

use crate::auth;
use crate::config;
use crate::data::{self, CommentService, FeedService, InteractionService, SubredditService};
use crate::media;
use crate::reddit;
use crate::session;
use crate::storage;
use crate::theme;
use crate::ui;

pub fn run() -> Result<()> {
    let cfg = config::load(config::LoadOptions::default()).context("load config")?;
    let config_path = config::default_path();
    let display_path = friendly_path(config_path.as_ref());

    ui::configure_terminal_cell_metrics_override(cfg.ui.cell_width, cfg.ui.cell_height);

    let store =
        Arc::new(storage::Store::open(storage::Options::default()).context("open storage")?);

    let media_cfg = media::Config {
        cache_dir: cfg.media.cache_dir.clone(),
        max_size_bytes: cfg.media.max_size_bytes,
        default_ttl: cfg.media.default_ttl,
        workers: cfg.media.workers,
        http_client: None,
        max_queue_depth: cfg.media.max_queue_depth,
    };
    let media_manager = media::Manager::new(store.clone(), media_cfg).ok();
    let media_handle = media_manager.as_ref().map(|manager| manager.handle());

    let theme = &cfg.ui.theme;
    let mut status = format!("Theme {theme} active. Press m for the guided menu or q to quit.");
    let mut content = format!(
        "Open the guided menu with m, then press a to add a Reddit account.\n\nConfigure reddit.client_id, reddit.client_secret (optional), and reddit.redirect_uri in {display_path} before authorizing."
    );
    let subreddits = vec![
        "r/frontpage".to_string(),
        "r/popular".to_string(),
        "r/programming".to_string(),
        "r/rust".to_string(),
    ];
    let mut posts: Vec<ui::PostPreview> = vec![
        placeholder_post(
            "connect",
            "Connect your Reddit account",
            "Press m to open the guided menu, then press a to start the Reddit sign-in flow.\nMake sure reddit.client_id and reddit.redirect_uri are set in the config file before you authorize.",
        ),
        placeholder_post(
            "shortcuts",
            "Keyboard shortcuts",
            "Use h and l to move between panes, j and k to move within a list, p to refresh posts, s to sync subreddit subscriptions, and q to quit.",
        ),
    ];

    let mut feed_service: Option<Arc<dyn data::FeedService + Send + Sync>> = None;
    let mut subreddit_service: Option<Arc<dyn data::SubredditService + Send + Sync>> = None;
    let mut comment_service: Option<Arc<dyn data::CommentService + Send + Sync>> = None;
    let mut interaction_service: Option<Arc<dyn data::InteractionService + Send + Sync>> = None;

    let mut session_manager: Option<Arc<session::Manager>> = None;
    let mut fetch_subreddits_on_start = false;

    let login_ready = !cfg.reddit.client_id.trim().is_empty()
        && !cfg.reddit.user_agent.trim().is_empty()
        && !cfg.reddit.redirect_uri.trim().is_empty();

    if login_ready {
        let flow_cfg = auth::Config {
            client_id: cfg.reddit.client_id.clone(),
            client_secret: cfg.reddit.client_secret.clone(),
            scope: cfg.reddit.scopes.clone(),
            user_agent: cfg.reddit.user_agent.clone(),
            auth_url: "https://www.reddit.com/api/v1/authorize".into(),
            token_url: "https://www.reddit.com/api/v1/access_token".into(),
            identity_url: "https://oauth.reddit.com/api/v1/me".into(),
            redirect_uri: cfg.reddit.redirect_uri.clone(),
            refresh_skew: std::time::Duration::from_secs(30),
        };

        if let Ok(flow) = auth::Flow::new(store.clone(), flow_cfg) {
            let flow = Arc::new(flow);
            if let Ok(raw_manager) = session::Manager::new(store.clone(), flow.clone()) {
                let manager = Arc::new(raw_manager);
                if manager.load_existing().is_ok() {
                    if let Some(session) = manager.active() {
                        let username = session.account.username.clone();
                        status = format!("Signed in as {username}. Press q to quit.");
                        content =
                            format!("Active account: {username}\nRemaining requests: loading...");

                        if let Ok(token_provider) = manager.active_token_provider() {
                            if let Ok(client) = reddit::Client::new(
                                token_provider,
                                reddit::ClientConfig {
                                    user_agent: cfg.reddit.user_agent.clone(),
                                    base_url: None,
                                    http_client: None,
                                },
                            ) {
                                let client = Arc::new(client);
                                let subreddit_api: Arc<dyn SubredditService + Send + Sync> =
                                    Arc::new(data::RedditSubredditService::new(client.clone()));
                                let feed_api: Arc<dyn FeedService + Send + Sync> =
                                    Arc::new(data::RedditFeedService::new(client.clone()));
                                let comment_api: Arc<dyn CommentService + Send + Sync> =
                                    Arc::new(data::RedditCommentService::new(client.clone()));
                                let interaction_api: Arc<dyn InteractionService + Send + Sync> =
                                    Arc::new(data::RedditInteractionService::new(client.clone()));

                                feed_service = Some(feed_api);
                                subreddit_service = Some(subreddit_api);
                                comment_service = Some(comment_api);
                                interaction_service = Some(interaction_api);
                                fetch_subreddits_on_start = true;
                                posts.clear();
                            }
                        }
                    } else {
                        status = "Ready to authorize a Reddit account. Press m then a to begin."
                            .to_string();
                        content = format!(
                            "Your Reddit API credentials were found, but no account is signed in yet.\nPress m to open the guided menu, choose Add account, and follow the authorization flow.\nConfig file: {display_path}"
                        );
                    }
                }
                session_manager = Some(manager);
            }
        }
    } else {
        status = format!(
            "Reddit credentials missing. Add reddit.client_id to {display_path} and press m to authorize."
        );
        content = format!(
            "Update {display_path} with a reddit.client_id (and optional reddit.client_secret).\nThen press m and choose Add account to sign in. Until then you can explore the interface using the built-in quickstart cards."
        );
    }

    let options = ui::Options {
        status_message: status,
        subreddits,
        posts,
        content,
        feed_service,
        subreddit_service,
        default_sort: reddit::SortOption::Hot,
        default_comment_sort: reddit::CommentSortOption::Confidence,
        comment_service,
        interaction_service,
        media_handle,
        config_path: display_path.clone(),
        store: store.clone(),
        session_manager: session_manager.clone(),
        fetch_subreddits_on_start,
        theme: theme::palette_for(theme),
    };

    let mut model = ui::Model::new(options);
    model.run()?;

    if let Some(manager) = session_manager {
        manager.close();
    }
    drop(media_manager);

    Ok(())
}

fn friendly_path(path: Option<&std::path::PathBuf>) -> String {
    if let Some(path) = path {
        if let Some(home) = dirs::home_dir() {
            if let Ok(stripped) = path.strip_prefix(&home) {
                let mut display = String::from("~");
                if !stripped.as_os_str().is_empty() {
                    display.push_str(&format!("/{}", stripped.display()));
                }
                return display;
            }
        }
        path.display().to_string()
    } else {
        "~/.config/reddix/config.yaml".to_string()
    }
}

fn placeholder_post(id: &str, title: &str, description: &str) -> ui::PostPreview {
    let body = format!("{title}\n\n{description}");
    let links = Vec::new();
    ui::PostPreview {
        title: title.to_string(),
        body,
        post: reddit::Post {
            id: id.to_string(),
            name: format!("t3_{id}"),
            title: title.to_string(),
            subreddit: "r/reddix".to_string(),
            author: "reddix".to_string(),
            selftext: description.to_string(),
            url: String::new(),
            permalink: format!("/r/reddix/{id}"),
            score: 0,
            likes: None,
            num_comments: 0,
            created_utc: 0.0,
            thumbnail: String::new(),
            stickied: false,
            over_18: false,
            spoiler: false,
            post_hint: String::new(),
            is_video: false,
            media: None,
            secure_media: None,
            crosspost_parent_list: Vec::new(),
            preview: reddit::Preview::default(),
            gallery_data: None,
            media_metadata: None,
        },
        links,
    }
}
