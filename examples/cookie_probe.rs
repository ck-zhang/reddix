use std::sync::Arc;

use anyhow::Result;
use reddix::reddit::{self, ListingOptions, SortOption, TokenProvider};

struct EnvTokenProvider {
    access_token: String,
}

impl TokenProvider for EnvTokenProvider {
    fn token(&self) -> Result<reddit::OAuthToken> {
        Ok(reddit::OAuthToken {
            access_token: self.access_token.clone(),
            token_type: "bearer".to_string(),
            expires_at: None,
        })
    }
}

fn main() -> Result<()> {
    let reddit_session = std::env::var("REDDIT_SESSION")?;
    let token_v2 = std::env::var("TOKEN_V2")?;
    let cookie_header = format!("reddit_session={reddit_session}; token_v2={token_v2}");
    let token_provider = Arc::new(EnvTokenProvider {
        access_token: token_v2,
    });
    let client = reddit::Client::new(
        token_provider,
        reddit::ClientConfig {
            user_agent: "reddix-cookie-probe/0.1".to_string(),
            base_url: Some("https://www.reddit.com/".to_string()),
            http_client: None,
            cookie_header: Some(cookie_header),
            bearer_auth: false,
        },
    )?;
    let listing = client.front_page(
        SortOption::Hot,
        ListingOptions {
            limit: Some(1),
            ..Default::default()
        },
    )?;
    println!("children={}", listing.children.len());
    Ok(())
}
