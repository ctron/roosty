use std::{path::Path, time::Duration as StdDuration};

use postgresql_embedded::{Settings, SettingsBuilder, VersionReq};

const EMBEDDED_POSTGRES_VERSION: &str = "=18.4.0";

/// Build embedded PostgreSQL settings with a fixed reusable installation.
///
/// Each caller owns an isolated cluster under a temporary directory. Teardown should close its
/// pools and stop the server, then let the directory guard remove the data; an explicit database
/// drop can race PostgreSQL backend disconnect cleanup under parallel test load.
pub(crate) fn settings(data_dir: &Path, password_file: std::path::PathBuf) -> Settings {
    SettingsBuilder::new()
        .version(VersionReq::parse(EMBEDDED_POSTGRES_VERSION).expect("valid PostgreSQL version"))
        .data_dir(data_dir)
        .password_file(password_file)
        .timeout(Some(StdDuration::from_secs(30)))
        .build()
}
