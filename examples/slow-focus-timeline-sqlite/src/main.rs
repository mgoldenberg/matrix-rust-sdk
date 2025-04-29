use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    ruma::{OwnedEventId, OwnedRoomId},
    Client,
};
use matrix_sdk_ui::timeline::{RoomExt, TimelineFocus};
use url::Url;

#[derive(Parser, Debug)]
struct Cli {
    /// The homeserver to connect to.
    #[clap(long)]
    homeserver: Url,

    /// The user name that should be used for the login.
    #[clap(long)]
    username: String,

    /// The password that should be used for the login.
    #[clap(long)]
    password: String,

    /// Set the proxy that should be used for the connection.
    #[clap(long)]
    proxy: Option<Url>,

    /// Enable verbose logging output.
    #[clap(long, action)]
    verbose: bool,

    /// The room id that we should listen for the,
    #[clap(long)]
    room_id: OwnedRoomId,

    #[clap(long)]
    event_id: OwnedEventId,

    #[clap(long, default_value = "20")]
    num_context_events: u16,

    /// Whether to use a SQLite store or an in-memory store
    #[clap(long, action)]
    sqlite_store: bool,

    #[clap(long, default_value = "db.sqlite")]
    sqlite_store_path: String,

    #[clap(long)]
    device_id: Option<String>,
}

async fn login(cli: Cli) -> Result<Client> {
    // Note that when encryption is enabled, you should use a persistent store to be
    // able to restore the session with a working encryption setup.
    // See the `persist_session` example.
    let mut builder = Client::builder().homeserver_url(cli.homeserver);

    if let Some(proxy) = cli.proxy {
        builder = builder.proxy(proxy);
    }

    if cli.sqlite_store {
        println!("Using SQLite store, path: {}", cli.sqlite_store_path);
        builder = builder.sqlite_store(cli.sqlite_store_path, None);
    }

    let client = builder.build().await?;

    let mut login_builder = client
        .matrix_auth()
        .login_username(&cli.username, &cli.password)
        .initial_device_display_name("rust-sdk");

    if let Some(device_id) = cli.device_id {
        login_builder = login_builder.device_id(device_id.as_str());
    }

    login_builder.await?;

    Ok(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let room_id = cli.room_id.clone();
    let event_id = cli.event_id.clone();
    let num_context_events = cli.num_context_events;
    let client = login(cli).await?;

    let sync_settings = SyncSettings::default();

    // Wait for the first sync response
    println!("Wait for the first sync");

    client.sync_once(sync_settings.clone()).await?;

    // Get the timeline stream and listen to it.
    let room = client.get_room(&room_id).unwrap();

    println!("Constructing live timeline... ");
    let timeline = room.timeline().await?;
    while !timeline.paginate_backwards(100).await? {
        println!("Paginating backwards...");
    }
    print!("Listening for timeline events... ");
    let (timeline_items, mut timeline_stream) = timeline.subscribe().await;
    println!("Initial timeline contains ({}) events", timeline_items.len());
    tokio::spawn(async move {
        while let Some(diffs) = timeline_stream.next().await {
            println!("Received timeline diffs: {diffs:?}");
        }
    });

    println!("Constructing event timeline... ");
    let focus = TimelineFocus::Event { target: event_id, num_context_events };
    let now = Instant::now();
    let _ = room.timeline_builder().with_focus(focus.clone()).build().await?;
    let elapsed = now.elapsed();
    println!("Event timeline (focus={focus:?}) took {elapsed:?} to construct");

    // Sync forever
    client.sync(sync_settings).await?;

    Ok(())
}
