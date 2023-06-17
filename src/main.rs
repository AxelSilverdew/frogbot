use anyhow;
use toml;
use tokio;
use scraper::{Html, Selector};
use lazy_static::lazy_static;
use regex::Regex;
use log::*;
use serde::{Serialize, Deserialize};
use matrix_sdk::{
    Client,
    config::SyncSettings,
    room::Room,

    ruma::OwnedDeviceId,
    ruma::OwnedRoomId,
    ruma::api::client::uiaa,
    ruma::events::room::member::StrippedRoomMemberEvent,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
};

#[derive(Serialize, Deserialize, Debug)]
struct TomlConfig {
    homeserver: String,
    username: String,
    display_name: String,
    password: String,
    room_ids: Vec<OwnedRoomId>,
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

    warn!("Logged in successfully!");
    warn!("server: '{}', username: '{}', display name: '{}'", &config.homeserver, &config.username, &config.display_name);

    // sync client once so we get latest events to work on before we continue
    client.sync_once(SyncSettings::default()).await?;
    
    warn!("Deleting old encryption devices");
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
    warn!("Finished deleting old encryption devices");
    warn!("Rejecting stale invites");
    for room in client.invited_rooms() {
        let room_name = room.name().unwrap_or_default();
        if !room.is_space() && !room.is_direct() && config.room_ids.iter().any(|r| *r == room.room_id()) {
            warn!("Got invite to room: '{}'", room_name);
            room.accept_invitation().await.expect("Failed to accept invite");
            warn!("Joining room!");
            if let Err(e) = client.join_room_by_id(room.room_id()).await {
                error!("Failed to join room with id: {} and error: {}", room.room_id(), e);
            }
        } else {
            warn!("Rejecting invite to room: '{}'", room_name);
            room.reject_invitation().await.unwrap_or_default();
        }
    }
    warn!("Finished rejecting stale invites");

    // Add handler to log new room invites as they're recieved
    client.add_event_handler(|ev: StrippedRoomMemberEvent, room: Room| async move {
        if let Room::Invited(invited_room) = room {
            warn!("Got invite to room: '{}' sent by '{}'", invited_room.name().unwrap_or_default(), ev.sender);
        }
    });

    // Add handler to detect and create embeds for HTTP links in chat
    client.add_event_handler(handle_message_events);

    async fn handle_message_events(ev: OriginalSyncRoomMessageEvent, room: Room, client: Client) {
        // Using lazy static magic here, so this means the regex is compiled exactly once
        // After initial compile it gets reused instead of recompiling on every message event
        lazy_static! {
            // shamelessly stolen and modified from some garbage blog online
            // I have no fucking idea how this works - https://urlregex.com/
            static ref RE: Regex = Regex::new(r"(?:(?:https?)://)(?:\S+(?::\S*)?@|\d{1,3}(?:\.\d{1,3}){3}|(?:(?:[a-z\d\x{00a1}-\x{ffff}]+-?)*[a-z\d\x{00a1}-\x{ffff}]+)(?:\.(?:[a-z\d\x{00a1}-\x{ffff}]+-?)*[a-z\d\x{00a1}-\x{ffff}]+)*(?:\.[a-z\x{00a1}-\x{ffff}]{2,6}))(?::\d+)?(?:[^\s]*)?").unwrap();
        }
        if let Room::Joined(room) = room {
            let full_reply_event = ev.clone().into_full_event(room.room_id().to_owned());
            let MessageType::Text(text_content) = ev.content.msgtype else {
                warn!("Ignoring message, content is not plaintext!");
                return;
            };
            // If the sender ID matches our client, ignore message
            // We don't want to reply to ourselves
            let client_user_id = client.user_id().unwrap();
            if ev.sender == client_user_id {
                return;
            }

            let msg = text_content.body.to_lowercase();
            // Make a HTTP request and parse out the metadata info
            if let Some(url) = RE.find(&msg) {
                if url.as_str().contains("localhost") || url.as_str().contains("127.0.0.1") {
                    warn!("This is probably a malicious URL, ignoring!");
                    return;
                }
                warn!("Got message with URL: '{}', requesting metadata!", url.as_str());
                if let Ok(req) = reqwest::get(url.as_str()).await {
                    if let Ok(resp) = req.text().await {
                        // beware dirty HTML parsing code
                        let (title, desc) = parse_metadata(&resp);
                        
                        // Build our message reply
                        let msg_reply = RoomMessageEventContent::text_plain(
                            format!("Title: {}\nDescription: {}", title, desc))
                            .make_reply_to(&full_reply_event);

                        // Finally send the reply to the room
                        warn!("Sending metadata for URL: '{}'", url.as_str());
                        if room.send(msg_reply, None).await.is_err() {
                            warn!("Failed to send metadata reply for URL: '{}'", url.as_str());
                        }
                    } else {
                        warn!("Failed to parse HTML response into text for URL: '{}'", url.as_str());
                    }
                } else {
                    warn!("Failed to get metadata for URL: '{}'", url.as_str());
                }
            } else {
                info!("Got message but found no URLs, ignoring");
            }
        }
    }

    fn parse_metadata(page: &String) -> (String, String) {
        let doc_body = Html::parse_document(page);
        
        // Selectors used to get metadata are defined here
        let title_selector = Selector::parse("title").unwrap();
        let description_selector = Selector::parse("meta[name=\"description\"]").unwrap();
        
        // Grab the actual data
        let title = doc_body.select(&title_selector).next();
        let desc = doc_body.select(&description_selector).next();
        // Clean up meta info and store it as a string
        let mut meta_title = String::from("None");
        let mut meta_description = String::from("None");

        if title.is_some() {
            meta_title = title.unwrap().text().collect();
        } else {
            warn!("Failed to parse title HTML");
        }

        if desc.is_some() {
            meta_description = desc.unwrap().value().attr("content").unwrap().to_string();
        } else {
            warn!("Failed to parse description HTML");
        }

        return (meta_title, meta_description);

    }
    
    // Now keep on syncing forever. `sync()` will use the latest sync token automatically.
    warn!("Starting sync loop");
    client.sync(SyncSettings::default()).await?; 
    Ok(())
}

fn load_config() -> TomlConfig {
    let config_file = std::fs::read_to_string("./config.toml").expect("Failed to read config file");
    let config: TomlConfig = toml::from_str(&config_file).expect("Failed to parse TOML config");
    return config;

}
