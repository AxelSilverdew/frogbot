//! # The Embed Module
//!
//! This module controls the embed functionality of frogbot.

use lazy_static::lazy_static;
use log::warn;
use matrix_sdk::{
    room::Room,
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
    },
    Client,
};
use regex::Regex;
use scraper::{Html, Selector};

/// Represents an Embed in the chat
#[derive(Default)]
pub struct Embed {
    /// The title of the embed
    pub title: String,
    /// The description
    pub description: String,
}

impl Embed {
    /// Creates a new [`Embed`].
    pub fn new(title: String, description: String) -> Embed {
        Embed { title, description }
    }
}

/// Scrapes the HTML of a webpage and generates an [`Embed`] with the scraped information.
pub fn parse_metadata(page: &str) -> Option<Embed> {
    let doc_body = Html::parse_document(page);

    // Selectors used to get metadata are defined here
    let title_selector = Selector::parse("title").unwrap();
    let description_selector = Selector::parse("meta[name=\"description\"]").unwrap();

    // Grab the actual data
    let title = doc_body.select(&title_selector).next();
    let desc = doc_body.select(&description_selector).next();
    // Clean up meta info and store it as a string
    let mut meta_title = String::default();
    let mut meta_description = String::default();

    match (title, desc) {
        // If both title and description aren't found return None
        (None, None) => {
            warn!("Couldn't parse any metadata for URL");
            return None;
        },
        // Otherwise set the title/description to whatever we find
        (Some(title), Some(desc)) => {
            meta_title = title.text().collect();
            meta_description = desc.value().attr("content").unwrap().to_string();
        }
        // Handle logging of parse failures
        (Some(_), None) => warn!("Failed to parse description HTML"),
        (None, Some(_)) => warn!("Failed to parse title HTML"),
    }

    Some(Embed::new(meta_title, meta_description))
}

/// Check if the message has any urls in it and get them if it does
fn get_urls_from_message(message: &str) -> Vec<&str> {
    // Using lazy static magic here, so this means the regex is compiled exactly once
    // After initial compile it gets reused instead of recompiling on every message event
    lazy_static! {
        // shamelessly stolen and modified from some garbage blog online
        // I have no fucking idea how this works - https://urlregex.com/
        static ref RE: Regex = Regex::new(r"(?:(?:https?)://)(?:\S+(?::\S*)?@|\d{1,3}(?:\.\d{1,3}){3}|(?:(?:[a-z\d\x{00a1}-\x{ffff}]+-?)*[a-z\d\x{00a1}-\x{ffff}]+)(?:\.(?:[a-z\d\x{00a1}-\x{ffff}]+-?)*[a-z\d\x{00a1}-\x{ffff}]+)*(?:\.[a-z\x{00a1}-\x{ffff}]{2,6}))(?::\d+)?(?:[^\s]*)?").unwrap();
    }

    // This will hold all the urls in the message if any are found
    let mut urls: Vec<&str> = vec![];

    if RE.is_match(message) {
        // If we find any urls, push them into the urls vec
        for regex_match in RE.find_iter(message) {
            // If the url points to localhost, we don't want to embed it, so we ignore it
            if regex_match.as_str().to_lowercase().contains("localhost")
                || regex_match.as_str().to_lowercase().contains("127.0.0.1")
            {
                warn!("This is probably a malicious URL, ignoring!");
            } else {
                warn!("Found {}", &regex_match.as_str());
                urls.push(regex_match.as_str());
            }
        }
    } else {
        // If we don't find any urls, do nothing
    };
    urls
}

/// Checks messages for valid links and generates embeds if found
pub async fn embed_handler(event: OriginalSyncRoomMessageEvent, room: Room, client: Client) {
    if let Room::Joined(room) = room {
        let full_reply_event = event.clone().into_full_event(room.room_id().to_owned());

        // If the sender ID matches our client, ignore the message
        // We don't want to reply to ourselves
        let client_user_id = client.user_id().unwrap();
        if event.sender == client_user_id {
            return;
        }

        // Do not make an embed if someone replies to a URL
        // Unfortunately, this makes it so that if your reply has a URL, it will not embed.
        // TODO: Fix this by scanning replies and only generating embeds for new URLs in future.
        if let Some(Relation::Reply { in_reply_to: _ }) = &event.content.relates_to {
            warn!("Ignoring message, it's a reply to someone else!");
            return;
        }

        // Ignore anything that isn't text
        let MessageType::Text(text_content) = event.content.msgtype else {
            warn!("Ignoring message, content is not plaintext!");
            return;
        };

        let urls = get_urls_from_message(&text_content.body);

        let reqwest_client = reqwest::Client::builder().user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36").build().unwrap();

        for mut url in urls {
            if let Ok(req) = reqwest_client.get(url).send().await {
                if let Ok(res) = req.text().await {
                    // beware, dirty HTML parsing code
                    let metadata = parse_metadata(&res);

                    // If we didn't get any metadata set URL to nothing so it won't get repeated
                    // With no other embed data in the bot's embed message
                    if metadata.is_none() {
                        url = "";
                    }

                    // Build our message reply
                    let embed = metadata.unwrap_or(Embed::new("No metadata found".to_string(), "".to_string()));
                    let bot_reply = RoomMessageEventContent::text_html(
                        &embed.title,
                        format!(
                            r#"
                                <blockquote>
                                    <h6><a href="{}">{}</a></h6>
                                    <h3><strong>{}</strong></h3>
                                    <p>{}</p>
                                </blockquote>
                        "#,
                            &url, &url, &embed.title, &embed.description
                        ),
                    )
                    .make_reply_to(&full_reply_event);

                    // Finally send the reply to the room
                    warn!("Sending embed for URL: '{}'", &url);
                    if room.send(bot_reply, None).await.is_err() {
                        warn!("Failed to send embed for URL: '{}'", &url);
                    }
                } else {
                    warn!("Failed to parse HTML for URL: '{}'", &url);
                }
            } else {
                warn!("Failed to get metadata for '{}'", &url);
            }
        }
    };
}
