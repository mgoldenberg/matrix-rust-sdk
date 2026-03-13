use anyhow::Context;
use matrix_sdk::{Client, config::SyncSettings};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let homeserver =
        std::env::var("MATRIX_HOMESERVER").unwrap_or(String::from("https://matrix.org"));
    let username = std::env::var("MATRIX_USERNAME").with_context(|| "Username not found!")?;
    let password = std::env::var("MATRIX_PASSWORD").with_context(|| "Password not found!")?;
    let db_path = std::env::var("MATRIX_SQLITE_DB_PATH").unwrap_or(format!("{username}.db"));
    let db_passphrase = std::env::var("MATRIX_SQLITE_DB_PASSPHRASE").ok();

    let client = Client::builder()
        .homeserver_url(&homeserver)
        .sqlite_store(&db_path, db_passphrase.as_deref())
        .build()
        .await?;
    client
        .matrix_auth()
        .login_username(&username, password.as_str())
        .initial_device_display_name("login-restore client")
        .device_id("login-restore")
        .await?;
    info!("Logged in as '@{username}:{homeserver}', database at '{db_path}'");

    let mut sync_settings =
        SyncSettings::default();
    let response = client.sync_once(sync_settings.clone()).await?;
    sync_settings = sync_settings.token(response.next_batch.clone());
    info!("Synced client and saved token ...");

    let session = client.matrix_auth().session().unwrap();
    info!("Saved session ...");

    drop(client);
    info!("Dropped client ...");

    let client = Client::builder()
        .homeserver_url(&homeserver)
        .sqlite_store(&db_path, db_passphrase.as_deref())
        .build()
        .await?;
    client.restore_session(session).await?;
    info!("Restored session in new client!");

    client.sync_once(sync_settings.clone()).await?;
    info!("Re-synced new client ...");

    client.logout().await?;
    info!("Logged out!");

    Ok(())
}
