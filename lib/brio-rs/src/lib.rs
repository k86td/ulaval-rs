use std::fmt::Debug;

use reqwest::header::{HeaderMap, HeaderValue};

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

const BRIO_BASE_URL: &str = "https://identites.brioeducation.ca";

pub struct SessionManager {
    bearer_token: String,
}

impl SessionManager {
    async fn get_userinfo(&self) -> Result<(), ()> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&self.bearer_token).unwrap(),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        let res = client
            .get(BRIO_BASE_URL.to_owned() + "/auth/oauth2/userinfo/")
            .send()
            .await;

        dbg!(res.unwrap().text().await);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the live Brio userinfo endpoint, so it needs a real token.
    /// Set BRIO_TEST_TOKEN to the opaque access token to run it:
    ///
    ///     BRIO_TEST_TOKEN=<token> cargo test -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires BRIO_TEST_TOKEN and network access"]
    async fn get_userinfo() {
        let token = std::env::var("BRIO_TEST_TOKEN").expect("BRIO_TEST_TOKEN must be set");

        let s = SessionManager {
            bearer_token: format!("Bearer {token}"),
        };

        dbg!(s.get_userinfo().await);
    }
}
