use anyhow::{anyhow, Result};
use clap::Parser;
use futures_util::StreamExt;
use matrix_sdk::{
    ruma::{
        api::client::room::create_room::v3::Request as CreateRoomRequest, DeviceId, RoomId, UserId,
    },
    sync::SyncResponse,
    Client, Room,
};

const INVITER_DEVICE: &'static str = "r";
const INVITED_DEVICE_1: &'static str = "d1";
const INVITED_DEVICE_2: &'static str = "d2";

#[derive(Parser, Debug)]
struct Cli {
    #[clap(long)]
    /// The homeserver of the users.
    homeserver_url: String,
    /// The user name of the inviter.
    #[clap(long)]
    inviter: String,
    /// The password of the inviter.
    #[clap(long)]
    inviter_password: String,
    /// The user name of the invited.
    #[clap(long)]
    invited: String,
    /// The password of the invited.
    #[clap(long)]
    invited_password: String,
}

#[derive(Debug)]
pub enum User {
    Inviter,
    Invited,
}

impl Cli {
    pub fn as_login_options<'a>(&'a self, user: User, device_id: &'a str) -> LoginOptions<'a> {
        match user {
            User::Inviter => LoginOptions {
                homeserver_url: &self.homeserver_url,
                user_id: &self.inviter,
                password: &self.inviter_password,
                device_id,
            },
            User::Invited => LoginOptions {
                homeserver_url: &self.homeserver_url,
                user_id: &self.invited,
                password: &self.invited_password,
                device_id,
            },
        }
    }
}

#[derive(Debug)]
pub struct LoginOptions<'a> {
    homeserver_url: &'a str,
    user_id: &'a str,
    password: &'a str,
    device_id: &'a str,
}

fn get_user_and_device(client: &Client) -> (&UserId, &DeviceId) {
    (client.user_id().expect("get user id"), client.device_id().expect("device_id"))
}

async fn login(options: LoginOptions<'_>) -> Result<Client> {
    let database = options.device_id.to_owned() + ".db";
    let client = Client::builder()
        .homeserver_url(options.homeserver_url)
        .sqlite_store(database, Some(options.password))
        .build()
        .await?;
    client
        .matrix_auth()
        .login_username(options.user_id, options.password)
        .device_id(options.device_id)
        .await?;
    client.sync_once(Default::default()).await?;
    let (user_id, device_id) = get_user_and_device(&client);
    tracing::info!("User '{user_id}' logged in via device '{device_id}'");
    Ok(client)
}

async fn logout(client: &Client) -> Result<()> {
    let (user_id, device_id) = get_user_and_device(&client);
    client.logout().await?;
    tracing::info!("User '{user_id}' logged out via device '{device_id}'");
    Ok(())
}

async fn wait_for_sync_response(
    client: &Client,
    f: impl Fn(&SyncResponse) -> bool,
) -> Result<SyncResponse> {
    let mut sync_stream = Box::pin(client.sync_stream(Default::default()).await);
    while let Some(response) = sync_stream.next().await.transpose()? {
        if f(&response) {
            return Ok(response);
        }
    }
    Err(anyhow!("Sync stream ended without matching response"))
}

async fn create_room_and_invite(client: &Client, invited_id: &UserId) -> Result<Room> {
    let mut create_room = CreateRoomRequest::new();
    create_room.invite = vec![invited_id.to_owned()];
    let room = client.create_room(create_room).await?;
    wait_for_sync_response(client, |response| response.rooms.joined.contains_key(room.room_id()))
        .await?;
    let inviter_id = client.user_id().expect("get inviter id");
    tracing::info!(
        "User '{inviter_id}' created room '{}' and invited '{invited_id}'",
        room.room_id()
    );
    Ok(room)
}

async fn join_room(client: &Client, room_id: &RoomId) -> Result<()> {
    client.join_room_by_id(room_id).await?;
    wait_for_sync_response(client, |response| response.rooms.joined.contains_key(room_id)).await?;
    let (user_id, device_id) = get_user_and_device(&client);
    tracing::info!("User '{user_id}' (device={device_id}) joined room '{room_id}'");
    Ok(())
}

async fn leave_room(client: &Client, room_id: &RoomId) -> Result<()> {
    client.get_room(room_id).ok_or(anyhow!("room not found"))?.leave().await?;
    wait_for_sync_response(client, |response| response.rooms.left.contains_key(room_id)).await?;
    let (user_id, device_id) = get_user_and_device(&client);
    tracing::info!("User '{user_id}' (device={device_id}) left room '{room_id}'");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // invited logs in via first device
    let invited = login(cli.as_login_options(User::Invited, INVITED_DEVICE_1)).await?;
    let invited_id = invited.user_id().unwrap();

    // inviter logs in, creates room, and invites invited
    let inviter = login(cli.as_login_options(User::Inviter, INVITER_DEVICE)).await?;
    let room = create_room_and_invite(&inviter, invited_id).await?;

    // invited waits for invitation via first device
    wait_for_sync_response(&invited, |response| {
        response.rooms.invited.contains_key(room.room_id())
    })
    .await?;

    // invited joins room via first device and then logs out
    join_room(&invited, room.room_id()).await?;
    logout(&invited).await?;

    // invited leaves room via second device and then logs out
    let invited = login(cli.as_login_options(User::Invited, INVITED_DEVICE_2)).await?;
    leave_room(&invited, room.room_id()).await?;
    logout(&invited).await?;

    // invited logs in via first device and queries joined rooms
    let invited = login(cli.as_login_options(User::Invited, INVITED_DEVICE_1)).await?;
    assert!(!invited.joined_rooms().into_iter().any(|r| r.room_id() == room.room_id()));

    Ok(())
}
