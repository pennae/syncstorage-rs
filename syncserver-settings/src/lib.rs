#[macro_use]
extern crate slog_scope;

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Deserializer};
use syncstorage_common::{
    X_LAST_MODIFIED, X_VERIFY_CODE, X_WEAVE_BYTES, X_WEAVE_NEXT_OFFSET, X_WEAVE_RECORDS,
    X_WEAVE_TIMESTAMP, X_WEAVE_TOTAL_BYTES, X_WEAVE_TOTAL_RECORDS,
};
use syncstorage_settings::Settings as SyncstorageSettings;
use tokenserver_settings::Settings as TokenserverSettings;
use url::Url;

pub static PREFIX: &str = "sync";

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: u16,
    pub host: String,
    pub actix_keep_alive: Option<u32>,
    /// The master secret, from which are derived
    /// the signing secret and token secret
    /// that are used during Hawk authentication.
    pub master_secret: Secrets,

    pub human_logs: bool,

    pub statsd_host: Option<String>,
    pub statsd_port: u16,

    /// Cors Settings
    pub cors_allowed_origin: Option<String>,
    pub cors_max_age: Option<usize>,
    pub cors_allowed_methods: Option<Vec<String>>,
    pub cors_allowed_headers: Option<Vec<String>>,

    // TOOD: Eventually, the below settings will be enabled or disabled via Cargo features
    pub syncstorage: SyncstorageSettings,
    pub tokenserver: TokenserverSettings,
}

impl Settings {
    /// Load the settings from the config file if supplied, then the environment.
    pub fn with_env_and_config_file(filename: Option<&str>) -> Result<Self, ConfigError> {
        let mut s = Config::default();

        // Merge the config file if supplied
        if let Some(config_filename) = filename {
            println!("merging with file {}", config_filename);
            s.merge(File::with_name(config_filename))?;
        }

        // Merge the environment overrides
        // While the prefix is currently case insensitive, it's traditional that
        // environment vars be UPPERCASE, this ensures that will continue should
        // Environment ever change their policy about case insensitivity.
        // This will accept environment variables specified as
        // `SYNC_FOO__BAR_VALUE="gorp"` as `foo.bar_value = "gorp"`
        s.merge(Environment::with_prefix(&PREFIX.to_uppercase()).separator("__"))?;

        s.try_into::<Self>().map_err(|e| {
            match e {
                // Configuration errors are not very sysop friendly, Try to make them
                // a bit more 3AM useful.
                ConfigError::Message(v) => {
                    println!("Bad configuration: {:?}", &v);
                    println!("Please set in config file or use environment variable.");
                    println!(
                        "For example to set `database_url` use env var `{}_DATABASE_URL`\n",
                        PREFIX.to_uppercase()
                    );
                    error!("Configuration error: Value undefined {:?}", &v);
                    ConfigError::NotFound(v)
                }
                _ => {
                    error!("Configuration error: Other: {:?}", &e);
                    e
                }
            }
        })
    }

    pub fn test_settings() -> Self {
        let mut settings =
            Self::with_env_and_config_file(None).expect("Could not get Settings in test_settings");
        settings.port = 8000;
        settings.syncstorage.database_pool_max_size = Some(1);
        settings.syncstorage.database_use_test_transactions = true;
        settings.syncstorage.database_pool_connection_max_idle = Some(300);
        settings.syncstorage.database_pool_connection_lifespan = Some(300);
        settings
    }

    pub fn banner(&self) -> String {
        let quota = if self.syncstorage.enable_quota {
            format!(
                "Quota: {} bytes ({}enforced)",
                self.syncstorage.limits.max_quota_limit,
                if !self.syncstorage.enforce_quota {
                    "un"
                } else {
                    ""
                }
            )
        } else {
            "No quota".to_owned()
        };
        let db = Url::parse(&self.syncstorage.database_url)
            .map(|url| url.scheme().to_owned())
            .unwrap_or_else(|_| "<invalid db>".to_owned());
        format!("http://{}:{} ({}) {}", self.host, self.port, db, quota)
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            port: 8000,
            host: "127.0.0.1".to_string(),
            actix_keep_alive: None,
            master_secret: Secrets::default(),
            statsd_host: Some("localhost".to_owned()),
            statsd_port: 8125,
            human_logs: false,
            cors_allowed_origin: None,
            cors_allowed_methods: Some(
                ["DELETE", "GET", "POST", "PUT"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            ),
            cors_allowed_headers: Some(
                [
                    "Authorization",
                    "Content-Type",
                    "UserAgent",
                    X_LAST_MODIFIED,
                    X_WEAVE_TIMESTAMP,
                    X_WEAVE_NEXT_OFFSET,
                    X_WEAVE_RECORDS,
                    X_WEAVE_BYTES,
                    X_WEAVE_TOTAL_RECORDS,
                    X_WEAVE_TOTAL_BYTES,
                    X_VERIFY_CODE,
                    "TEST_IDLES",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
            ),
            cors_max_age: None,
            syncstorage: SyncstorageSettings::default(),
            tokenserver: TokenserverSettings::default(),
        }
    }
}

/// Secrets used during Hawk authentication.
#[derive(Clone, Debug)]
pub struct Secrets {
    /// The master secret in byte array form.
    ///
    /// The signing secret and token secret are derived from this.
    pub master_secret: Vec<u8>,

    /// The signing secret used during Hawk authentication.
    pub signing_secret: [u8; 32],
}

impl Secrets {
    /// Decode the master secret to a byte array
    /// and derive the signing secret from it.
    pub fn new(master_secret: &str) -> Result<Self, String> {
        let master_secret = master_secret.as_bytes().to_vec();
        let signing_secret = syncstorage_common::hkdf_expand_32(
            b"services.mozilla.com/tokenlib/v1/signing",
            None,
            &master_secret,
        )?;
        Ok(Self {
            master_secret,
            signing_secret,
        })
    }
}

impl Default for Secrets {
    /// Create a (useless) default `Secrets` instance.
    fn default() -> Self {
        Self {
            master_secret: vec![],
            signing_secret: [0u8; 32],
        }
    }
}

impl<'d> Deserialize<'d> for Secrets {
    /// Deserialize the master secret and signing secret byte arrays
    /// from a single master secret string.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'d>,
    {
        let master_secret: String = Deserialize::deserialize(deserializer)?;
        Secrets::new(&master_secret)
            .map_err(|e| serde::de::Error::custom(format!("error: {:?}", e)))
    }
}
