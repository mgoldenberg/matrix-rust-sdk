use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    ruma::{
        api::client::room::create_room::v3::Request,
        events::{room::message::RoomMessageEventContent, AnySyncTimelineEvent},
    },
    Client,
};
use matrix_sdk_ui::timeline::{default_event_filter, RoomExt};
use url::Url;

#[derive(Parser, Debug)]
struct Cli {
    /// The homeserver to connect to.
    #[clap(value_parser)]
    homeserver: Url,

    /// The user name that should be used for the login.
    #[clap(value_parser)]
    user_name: String,

    /// The password that should be used for the login.
    #[clap(value_parser)]
    password: String,

    /// Set the proxy that should be used for the connection.
    #[clap(short, long)]
    proxy: Option<Url>,

    /// Enable verbose logging output.
    #[clap(short, long, action)]
    verbose: bool,

    /// An initial message to send in the room prior to subscribing to timeline
    #[clap(short, long)]
    initial_message: Option<String>,
}

async fn login(cli: Cli) -> Result<Client> {
    // Note that when encryption is enabled, you should use a persistent store to be
    // able to restore the session with a working encryption setup.
    // See the `persist_session` example.
    let mut builder = Client::builder().homeserver_url(cli.homeserver);

    if let Some(proxy) = cli.proxy {
        builder = builder.proxy(proxy);
    }

    let client = builder.build().await?;

    client
        .matrix_auth()
        .login_username(&cli.user_name, &cli.password)
        .initial_device_display_name("rust-sdk")
        .await?;

    Ok(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let initial_message = cli.initial_message.clone();
    let client = login(cli).await?;

    // Create a new empty room
    let room = client.create_room(Request::new()).await?;
    println!("Created new room: {}", room.room_id());

    // Send initial message into room, if provided
    if let Some(initial_message) = initial_message {
        println!("Sending message into room: {initial_message}");
        room.send(RoomMessageEventContent::text_plain(initial_message)).await?;
    }

    let sync_settings = SyncSettings::default();

    // Wait for the first sync response
    println!("Wait for the first sync");
    client.sync_once(sync_settings.clone()).await?;

    // Get the timeline stream and filter out events we don't care about
    let timeline = room
        .timeline_builder()
        .event_filter(|event, room_version| {
            default_event_filter(event, room_version)
                && matches!(event, AnySyncTimelineEvent::MessageLike(_))
        })
        .build()
        .await?;

    // Paginate back to the start of the timeline
    while !timeline.paginate_backwards(10).await? {
        println!("Paginating...");
    }

    let (timeline_items, mut timeline_stream) = timeline.subscribe().await;
    println!("Initial timeline items: {timeline_items:#?}");

    tokio::spawn(async move {
        while let Some(diffs) = timeline_stream.next().await {
            println!("Received timeline diffs: {diffs:#?}");
            let latest_event = timeline.latest_event().await;
            println!("latest_event={latest_event:?}");
        }
    });

    tokio::spawn(async move {
        room.send(RoomMessageEventContent::text_plain("hello")).await.unwrap();
    });

    // Sync forever
    client.sync(sync_settings).await?;

    Ok(())
}
