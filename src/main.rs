use anyhow;
use toml;
use tokio;
use log::*;
use serde::{Serialize, Deserialize};
use matrix_sdk::{
    Client,
    config::SyncSettings,
    room::Room,

    ruma::OwnedDeviceId,
    ruma::api::client::uiaa,
    ruma::events::room::member::StrippedRoomMemberEvent,
};

#[derive(Serialize, Deserialize, Debug)]
struct TomlConfig {
    homeserver: String,
    username: String,
    display_name: String,
    password: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // init logging
    tracing_subscriber::fmt::init();
    let config = load_config();
    let client = Client::builder()
        .homeserver_url(&config.homeserver)
        .handle_refresh_tokens()
        .build()
        .await?;
    
    // try login
    client
        .login_username(&config.username, &config.password)
        .initial_device_display_name(&config.display_name)
        .send()
        .await?;
    
    info!("Logged in successfully!");
    info!("server: '{}', username: '{}', display name: '{}'", &config.homeserver, &config.username, &config.display_name);

    // sync client once so we get latest events to work on before we continue
    client.sync_once(SyncSettings::default()).await?;
    
    info!("Deleting old encryption devices");
    let current_device_id = client.device_id().expect("Failed to get device ID");
    let old_devices: Vec<OwnedDeviceId> = client.devices().await?.devices.iter().filter(|d| d.device_id != current_device_id).map(|d| d.device_id.to_owned()).collect();
    
    // Deleting these devices needs "user interaction" or something, so we just send password again
    // and it works :D
    if let Err(e) = client.delete_devices(&old_devices, None).await {
        if let Some(info) = e.uiaa_response() {
            let mut password = uiaa::Password::new(
                uiaa::UserIdentifier::UserIdOrLocalpart(&config.username),
                &config.password,
            );
            password.session = info.session.as_deref();
            client
                .delete_devices(&old_devices, Some(uiaa::AuthData::Password(password)))
                .await?;
        }
    }
    info!("Finished deleting old encryption devices");
    info!("Rejecting stale invites");
    for room in client.invited_rooms() {
        room.reject_invitation().await.unwrap_or_default();
    }
    info!("Finished rejecting stale invites");

    // Add handler to deal with new room invites
    // TODO: Add code to filter rooms and only accept invites for rooms in config file
    client.add_event_handler(|ev: StrippedRoomMemberEvent, room: Room, client: Client| async move {
        info!("Processing room member event, room type: {:?}", room.room_type());
        if let Room::Invited(invited_room) = room {
            let room_name = ev.content.displayname.unwrap_or(String::from(""));
            let is_dm = ev.content.is_direct.unwrap_or(true);
            let is_bad_room = is_dm || invited_room.is_space() || room_name.is_empty();
            info!("Got invite to room: '{}' sent by '{}'", room_name, ev.sender);
            if is_bad_room {
                info!("This room is probably a DM, ignoring!");
                if let Err(e) = invited_room.reject_invitation().await {
                    warn!("Failed to reject invite with error: {}", e);
                }
                return ();
            } else {
                if let Err(e) = invited_room.accept_invitation().await {
                    warn!("Failed to accept room invite with error: {}", e);
                }
                info!("Joining room!");
                if let Err(e) = client.join_room_by_id(invited_room.room_id()).await {
                    warn!("Failed to join room with id: {} and error: {}", invited_room.room_id(), e);
                }
            }
        }
    });
    
    // Now keep on syncing forever. `sync()` will use the latest sync token automatically.
    info!("Starting sync loop");
    client.sync(SyncSettings::default()).await?; 
    Ok(())
}

fn load_config() -> TomlConfig {
    // fuck error handling, it's too early in the program execution for that shit
    let config: TomlConfig = toml::from_str(&std::fs::read_to_string("./config.toml").unwrap()).unwrap();
    return config; // see, so clean!

}
