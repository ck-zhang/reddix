use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const DEFAULT_ENV_PREFIX: &str = "REDDIX";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Config {
    #[serde(default)]
    pub reddit: RedditConfig,
    #[serde(default)]
    pub ui: UIConfig,
    #[serde(default)]
    pub media: MediaConfig,
    #[serde(default)]
    pub player: PlayerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RedditConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    #[serde(default = "default_redirect_uri")]
    pub redirect_uri: String,
}

impl Default for RedditConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            user_agent: default_user_agent(),
            scopes: default_scopes(),
            redirect_uri: default_redirect_uri(),
        }
    }
}

fn default_user_agent() -> String {
    "reddix-dev/0.1 (+https://github.com/ck-zhang/reddix)".to_string()
}

fn default_scopes() -> Vec<String> {
    vec![
        "identity".into(),
        "mysubreddits".into(),
        "read".into(),
        "vote".into(),
        "submit".into(),
        "subscribe".into(),
    ]
}

fn default_redirect_uri() -> String {
    "http://127.0.0.1:65010/reddix/callback".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UIConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub cell_width: Option<f64>,
    #[serde(default)]
    pub cell_height: Option<f64>,
}

impl Default for UIConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            cell_width: None,
            cell_height: None,
        }
    }
}

fn default_theme() -> String {
    "default".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaConfig {
    #[serde(default = "default_cache_dir")]
    pub cache_dir: Option<PathBuf>,
    #[serde(default = "default_max_size_bytes")]
    pub max_size_bytes: i64,
    #[serde(default = "default_media_ttl_duration", with = "humantime_serde")]
    pub default_ttl: Duration,
    #[serde(default = "default_workers")]
    pub workers: usize,
    #[serde(default = "default_media_queue_depth")]
    pub max_queue_depth: usize,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            max_size_bytes: default_max_size_bytes(),
            default_ttl: default_media_ttl_duration(),
            workers: default_workers(),
            max_queue_depth: default_media_queue_depth(),
        }
    }
}

fn default_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join("reddix"))
}

fn default_max_size_bytes() -> i64 {
    500 * 1024 * 1024
}

fn default_media_ttl_duration() -> Duration {
    Duration::from_secs(6 * 60 * 60)
}

fn default_media_queue_depth() -> usize {
    16
}

fn default_workers() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayerConfig {
    #[serde(default = "default_video_command")]
    pub video_command: Vec<String>,
    #[serde(default = "default_video_detach")]
    pub video_detach: bool,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            video_command: default_video_command(),
            video_detach: default_video_detach(),
        }
    }
}

fn default_video_command() -> Vec<String> {
    vec!["mpv".into(), "--fs".into(), "%URL%".into()]
}

fn default_video_detach() -> bool {
    true
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    pub config_file: Option<PathBuf>,
    pub env_prefix: Option<String>,
}

pub fn load(options: LoadOptions) -> Result<Config> {
    let mut cfg = Config::default();

    if let Some(path) = options.config_file.as_ref() {
        if path.exists() {
            let from_file = read_config_file(path)?;
            cfg = merge_config(cfg, from_file);
        }
    } else if let Some(default_path) = default_config_path() {
        if default_path.exists() {
            let from_file = read_config_file(&default_path)?;
            cfg = merge_config(cfg, from_file);
        }
    }

    let prefix = options.env_prefix.as_deref().unwrap_or(DEFAULT_ENV_PREFIX);
    cfg = merge_config(cfg, load_env(prefix)?);

    Ok(cfg)
}

fn read_config_file(path: &Path) -> Result<Config> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file at {}", path.display()))?;
    let config: Config = serde_yaml::from_str(&data)
        .with_context(|| format!("Failed to parse config file at {}", path.display()))?;
    Ok(config)
}

fn merge_config(mut base: Config, other: Config) -> Config {
    if !other.reddit.client_id.is_empty() {
        base.reddit.client_id = other.reddit.client_id;
    }
    if !other.reddit.client_secret.is_empty() {
        base.reddit.client_secret = other.reddit.client_secret;
    }
    if !other.reddit.user_agent.is_empty() {
        base.reddit.user_agent = other.reddit.user_agent;
    }
    if !other.reddit.scopes.is_empty() {
        base.reddit.scopes = other.reddit.scopes;
    }
    if !other.reddit.redirect_uri.is_empty() {
        base.reddit.redirect_uri = other.reddit.redirect_uri;
    }

    if !other.ui.theme.is_empty() {
        base.ui.theme = other.ui.theme;
    }
    if other.ui.cell_width.is_some() {
        base.ui.cell_width = other.ui.cell_width;
    }
    if other.ui.cell_height.is_some() {
        base.ui.cell_height = other.ui.cell_height;
    }

    if other.media.cache_dir.is_some() {
        base.media.cache_dir = other.media.cache_dir;
    }
    if other.media.max_size_bytes != 0 {
        base.media.max_size_bytes = other.media.max_size_bytes;
    }
    base.media.default_ttl = other.media.default_ttl;
    if other.media.workers != 0 {
        base.media.workers = other.media.workers;
    }
    if other.media.max_queue_depth != 0 {
        base.media.max_queue_depth = other.media.max_queue_depth;
    }

    if !other.player.video_command.is_empty() {
        base.player.video_command = other.player.video_command;
    }
    base.player.video_detach = other.player.video_detach;

    base
}

fn load_env(prefix: &str) -> Result<Config> {
    let mut map: HashMap<String, String> = HashMap::new();
    let upper_prefix = format!("{}_", prefix.to_uppercase());

    for (key, value) in env::vars() {
        if let Some(stripped) = key.strip_prefix(&upper_prefix) {
            let normalized = stripped.to_ascii_lowercase().replace("__", ".");
            map.insert(normalized, value);
        }
    }

    if map.is_empty() {
        return Ok(Config::default());
    }

    let mut cfg = Config::default();

    for (key, value) in map {
        apply_env_value(&mut cfg, &key, value);
    }

    Ok(cfg)
}

fn apply_env_value(cfg: &mut Config, key: &str, value: String) {
    match key {
        "reddit.client_id" => cfg.reddit.client_id = value,
        "reddit.client_secret" => cfg.reddit.client_secret = value,
        "reddit.user_agent" => cfg.reddit.user_agent = value,
        "reddit.redirect_uri" => cfg.reddit.redirect_uri = value,
        "reddit.scopes" => {
            cfg.reddit.scopes = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        "ui.theme" => cfg.ui.theme = value,
        "ui.cell_width" => {
            if let Ok(parsed) = value.parse::<f64>() {
                cfg.ui.cell_width = Some(parsed);
            }
        }
        "ui.cell_height" => {
            if let Ok(parsed) = value.parse::<f64>() {
                cfg.ui.cell_height = Some(parsed);
            }
        }
        "media.cache_dir" => cfg.media.cache_dir = Some(PathBuf::from(value)),
        "media.max_size_bytes" => {
            if let Ok(parsed) = value.parse::<i64>() {
                cfg.media.max_size_bytes = parsed;
            }
        }
        "media.default_ttl" => {
            if let Ok(duration) = humantime::parse_duration(&value) {
                cfg.media.default_ttl = duration;
            }
        }
        "media.workers" => {
            if let Ok(parsed) = value.parse::<usize>() {
                cfg.media.workers = parsed;
            }
        }
        "player.video_command" => {
            cfg.player.video_command = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        "player.video_detach" => {
            cfg.player.video_detach = matches!(value.as_str(), "1" | "true" | "TRUE" | "True");
        }
        _ => {}
    }
}

pub fn default_path() -> Option<PathBuf> {
    default_config_path()
}

fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("reddix").join("config.yaml"))
}

pub fn save_reddit_credentials(
    path: Option<PathBuf>,
    client_id: &str,
    client_secret: &str,
    user_agent: &str,
) -> Result<PathBuf> {
    let client_id = client_id.trim();
    let client_secret = client_secret.trim();
    let user_agent = user_agent.trim();

    anyhow::ensure!(
        !client_id.is_empty(),
        "config: reddit.client_id is required"
    );
    anyhow::ensure!(
        !user_agent.is_empty(),
        "config: reddit.user_agent is required"
    );

    let path = if let Some(path) = path {
        path
    } else {
        default_config_path().context("config: unable to determine default config path")?
    };

    let mut cfg = if path.exists() {
        read_config_file(&path)?
    } else {
        Config::default()
    };

    cfg.reddit.client_id = client_id.to_string();
    cfg.reddit.client_secret = client_secret.to_string();
    cfg.reddit.user_agent = user_agent.to_string();
    if cfg.reddit.scopes.is_empty() {
        cfg.reddit.scopes = default_scopes();
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("config: failed to create directory {}", parent.display()))?;
    }

    let contents = serde_yaml::to_string(&cfg).context("config: failed to serialize config")?;
    fs::write(&path, contents)
        .with_context(|| format!("config: failed to write file {}", path.display()))?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::tempdir;

    #[test]
    fn load_defaults_without_files() {
        let cfg = load(LoadOptions::default()).unwrap();
        assert_eq!(cfg.ui.theme, "default");
        assert!(cfg.ui.cell_width.is_none());
        assert!(cfg.ui.cell_height.is_none());
        assert_eq!(cfg.reddit.redirect_uri, default_redirect_uri());
    }

    #[test]
    fn save_credentials_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        save_reddit_credentials(Some(path.clone()), "client", "secret", "agent/1.0").unwrap();
        let saved = read_config_file(&path).unwrap();
        assert_eq!(saved.reddit.client_id, "client");
    }

    #[test]
    fn env_overrides() {
        env::set_var("REDDIX_UI__THEME", "dracula");
        env::set_var("REDDIX_UI__CELL_WIDTH", "8.5");
        env::set_var("REDDIX_UI__CELL_HEIGHT", "16.25");
        let cfg = load(LoadOptions::default()).unwrap();
        assert_eq!(cfg.ui.theme, "dracula");
        assert_eq!(cfg.ui.cell_width, Some(8.5));
        assert_eq!(cfg.ui.cell_height, Some(16.25));
        env::remove_var("REDDIX_UI__THEME");
        env::remove_var("REDDIX_UI__CELL_WIDTH");
        env::remove_var("REDDIX_UI__CELL_HEIGHT");
    }
}
