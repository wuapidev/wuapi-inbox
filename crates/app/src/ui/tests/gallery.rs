//! Pictures sent together stand as one album, and a chat's pictures,
//! files and links are listed from its panel.

use super::*;
use client_provider::{
    ContactId, Media, MediaKind, MediaRef, Message, MessageExtras, MessageId, ProviderEvent,
    Timestamp,
};

fn open_first(cx: &mut TestAppContext) -> Harness {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    cx.run_until_parked();
    harness
}

/// A message with a file from the other side, `at` milliseconds after
/// the newest there can be.
fn push(harness: &Harness, cx: &mut TestAppContext, id: &str, at: i64, media: Media) {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let message = Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new("them"),
        sender_name: None,
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 600_000 + at),
        content: MessageContent::Media(media),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: MessageExtras::default(),
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    harness.settle(cx);
}

fn photo(harness: &Harness, name: &str) -> Media {
    let url = format!("https://media.example/{name}.png");
    harness.mock.set_media(&url, png(64, 48), "image/png");
    let mut media = Media::new(MediaKind::Image);
    media.mime_type = Some("image/png".into());
    media.source = Some(MediaRef::new(url));
    media
}

fn folded(harness: &Harness, cx: &mut TestAppContext, id: &str) -> (bool, usize) {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Message(row) if row.stored.message.id.as_str() == id => {
                    Some((row.folded, row.album.len()))
                }
                _ => None,
            })
            .expect("the message is a row")
    })
}

#[gpui_kit::test]
fn pictures_sent_together_are_one_album_and_one_sent_later_is_not(cx: &mut TestAppContext) {
    let harness = open_first(cx);
    for (index, name) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        let media = photo(&harness, name);
        push(&harness, cx, name, index as i64 * 1_000, media);
    }
    // The last carries the album; the others are rows that draw nothing.
    assert_eq!(folded(&harness, cx, "a"), (true, 0));
    assert_eq!(folded(&harness, cx, "d"), (true, 0));
    assert_eq!(folded(&harness, cx, "e"), (false, 4));
    assert!(shows(harness.window, "album", cx));
    assert!(shows(harness.window, "album-more", cx), "five: four and +1");

    assert!(shows(harness.window, "media-image", cx));

    // Two minutes later: a picture of its own.
    let late = photo(&harness, "late");
    push(&harness, cx, "late", 4_000 + 120_000, late);
    assert_eq!(folded(&harness, cx, "late"), (false, 0));
    assert_eq!(folded(&harness, cx, "e"), (false, 4));

    // One with something to say stands alone too.
    let mut said = photo(&harness, "said");
    said.caption = Some("look".into());
    push(&harness, cx, "said", 4_000 + 121_000, said);
    assert_eq!(folded(&harness, cx, "said"), (false, 0));
    assert_eq!(folded(&harness, cx, "late"), (false, 0));
}

#[gpui_kit::test]
fn the_panel_of_a_chat_lists_its_pictures_files_and_links(cx: &mut TestAppContext) {
    let harness = open_first(cx);
    let media = photo(&harness, "kept");
    push(&harness, cx, "kept", 0, media);
    let mut file = Media::new(MediaKind::Document);
    file.file_name = Some("contract.pdf".into());
    file.source = Some(MediaRef::new("https://media.example/contract.pdf"));
    file.size_bytes = Some(120_000);
    push(&harness, cx, "file", 200_000, file);

    click(harness.window, "header-avatar", cx);
    assert!(shows(harness.window, "contact-info", cx));
    click(harness.window, "profile-gallery", cx);
    assert!(shows(harness.window, "gallery", cx));
    assert!(shows(harness.window, "gallery-cell", cx));
    click(harness.window, "gallery-tab-files", cx);
    assert!(shows(harness.window, "gallery-line", cx));
    assert!(!shows(harness.window, "gallery-cell", cx));

    // "Show in chat" closes the panel on the message.
    click(harness.window, "gallery-show", cx);
    assert!(!shows(harness.window, "contact-info", cx));

    // And back from the gallery is the panel it was opened from.
    click(harness.window, "header-avatar", cx);
    click(harness.window, "profile-gallery", cx);
    click(harness.window, "info-back", cx);
    assert!(!shows(harness.window, "gallery", cx));
    assert!(shows(harness.window, "profile-gallery", cx));
}
