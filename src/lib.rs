//! A multi-purpose bot for Matrix
#![deny(missing_docs)]
pub mod embeds;

use log::{error, warn};
use matrix_sdk::{
    config::SyncSettings,
    room::Room,
    ruma::{
        api::client::uiaa, events::room::member::StrippedRoomMemberEvent, OwnedDeviceId,
        OwnedRoomId,
    },
    Client, ClientBuildError,
};
use serde::{Deserialize, Serialize};

/// Represents the entries in the configuration file.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    /// Your Homeserver URL (e.g. "matrix.yourdomain.com")
    pub homeserver: String,
    /// The Bot User's Username (e.g. "frogbot")
    pub username: String,
    /// The Display Name of the Bot (e.g. "Frogbot 🐸")
    pub display_name: String,
    /// The Password to the Bot User (e.g. "hunter2")
    pub password: String,
    /// A List of All the Rooms to Join (e.g. ["!myid:matrix.yourdomain.com"] )
    pub room_ids: Vec<OwnedRoomId>,
}

impl Config {
    /// Loads a config file for frogbot to use.
    pub fn load(config_file: &str) -> Config {
        let config_file =
            std::fs::read_to_string(config_file).expect("Failed to read config file.");
        toml::from_str(&config_file).expect("Failed to parse TOML config.")
    }

    /// Returns a new frogbot client using the [`Config`].
    pub async fn create_client(&self) -> Result<Client, ClientBuildError> {
        Client::builder()
            .homeserver_url(&self.homeserver)
            .handle_refresh_tokens()
            .build()
            .await
    }
}

/// Deletes all old encryption devices.
///
/// We don't want to end up with a ton of encryption devices that aren't active.
/// This function removes all the old ones while preserving the current device.
///
/// # Panics
///
/// This function will panic if it cannot get a device ID from the current client.
pub async fn delete_old_encryption_devices(client: &Client, config: &Config) -> anyhow::Result<()> {
    warn!("Deleting old encryption devices");
    let current_device_id = client.device_id().expect("Failed to get device ID");
    let old_devices: Vec<OwnedDeviceId> = client
        .devices()
        .await?
        .devices
        .iter()
        .filter(|d| d.device_id != current_device_id)
        .map(|d| d.device_id.to_owned())
        .collect();

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
    warn!("Finished deleting old encryption devices");
    Ok(())
}

/// Rejects invites that aren't valid anymore or have timed out.
pub async fn reject_stale_invites(client: &Client, config: &Config) {
    warn!("Rejecting stale invites");
    for room in client.invited_rooms() {
        let room_name = room.name().unwrap_or_default();
        if !room.is_space()
            && !room.is_direct()
            && config.room_ids.iter().any(|r| *r == room.room_id())
        {
            warn!("Got invite to room: '{}'", room_name);
            room.accept_invitation()
                .await
                .expect("Failed to accept invite");
            warn!("Joining room!");
            if let Err(e) = client.join_room_by_id(room.room_id()).await {
                error!(
                    "Failed to join room with id: {} and error: {}",
                    room.room_id(),
                    e
                );
            }
        } else {
            warn!("Rejecting invite to room: '{}'", room_name);
            room.reject_invitation().await.unwrap_or_default();
        }
    }
    warn!("Finished rejecting stale invites");
}

/// Run frogbot
///
/// Starts the bot and starts listening for events
///
/// # Panics
///
/// This function will panic in the following scenarios:
/// - If it cannot create a client using the current [`Config`].
/// - If the bot can't log into it's account.
/// - If the initial event sync fails.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let client = &config
        .create_client()
        .await
        .expect("There was a problem creating frogbot's client.");

    // Attempt to log into the server
    client
        .login_username(&config.username, &config.password)
        .initial_device_display_name(&config.display_name)
        .send()
        .await
        .expect("frogbot couldn't log into it's account.");

    warn!("Logged in successfully!");
    warn!(
        "server: '{}', username: '{}', display name: '{}'",
        &config.homeserver, &config.username, &config.display_name
    );

    // sync client once so we get latest events to work on before we continue
    client
        .sync_once(SyncSettings::default())
        .await
        .expect("Failed the initial event sync.");

    delete_old_encryption_devices(client, &config).await?;

    reject_stale_invites(client, &config).await;

    // Add handler to log new room invites as they're recieved
    client.add_event_handler(|ev: StrippedRoomMemberEvent, room: Room| async move {
        if let Room::Invited(invited_room) = room {
            warn!(
                "Got invite to room: '{}' sent by '{}'",
                invited_room.name().unwrap_or_default(),
                ev.sender
            );
        }
    });

    // Add handler to detect and create embeds for HTTP links in chat
    client.add_event_handler(embeds::embed_handler);

    // Now keep on syncing forever. `sync()` will use the latest sync token automatically.
    warn!("Starting sync loop");
    client.sync(SyncSettings::default()).await?;

    Ok(())
}
