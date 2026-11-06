//! The shared state of all handlers.

use ruum_core::Services;

/// Settings that the API needs at run time.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// The public base URL, for example `https://ruum.localhost`.
    pub public_url: String,
    /// The base URL for share links and downloads on the user content domain.
    pub content_url: String,
    /// The URL of the WebDAV host (decision 0002-3), for example `https://dav.ruum.localhost/`.
    /// `createAppPassword` returns it as `dav_url`.
    pub dav_url: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            public_url: "https://ruum.localhost".into(),
            content_url: "https://content.ruumapps.localhost".into(),
            dav_url: "https://dav.ruum.localhost/".into(),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub services: Services,
    pub config: ApiConfig,
}

impl AppState {
    pub fn new(services: Services, config: ApiConfig) -> Self {
        Self { services, config }
    }

    /// All in-memory services and the default config, for tests.
    pub fn in_memory() -> Self {
        Self::new(Services::in_memory(), ApiConfig::default())
    }
}
