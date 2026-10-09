//! Deterministic seed data: two accounts, a few dozen chats, long threads.

use client_provider::{
    Account, AccountId, Chat, ChatId, ChatKind, ConnectionState, ContactId, DeliveryStatus,
    Direction, Media, MediaKind, MediaRef, Message, MessageContent, MessageId, ReplyRef, Timestamp,
};
use std::collections::HashMap;

/// A small deterministic generator, so the same seed always produces the
/// same world and tests can rely on it.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(crate) fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 16
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub(crate) fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const PEOPLE: &[&str] = &[
    "Valentina Rojas",
    "Daniel Okafor",
    "Mamá",
    "Sofía Méndez",
    "Lucas Ferreira",
    "Priya Raman",
    "Tomás Herrera",
    "Camila Duarte",
    "Noah Fischer",
    "Isabella Conti",
    "Andrés Villalba",
    "Hana Kobayashi",
    "Mateo Salazar",
    "Olivia Bennett",
    "Rafael Pinto",
    "Aisha Rahman",
    "Diego Marín",
    "Emma Lindqvist",
    "Javier Paredes",
    "Chloé Martin",
    "Samuel Adeyemi",
    "Laura Giménez",
    "Kenji Watanabe",
    "Paula Cárdenas",
    "Ethan Brooks",
    "Dr. Morales",
    "Gabriela Suárez",
    "Marco Bianchi",
];

const GROUPS: &[(&str, &[&str])] = &[
    (
        "Family",
        &["Mamá", "Papá", "Abuela Carmen", "Sofía Méndez", "Tío Luis"],
    ),
    (
        "Weekend football ⚽",
        &[
            "Diego Marín",
            "Rafael Pinto",
            "Tomás Herrera",
            "Samuel Adeyemi",
            "Marco Bianchi",
            "Javier Paredes",
        ],
    ),
    (
        "Design team",
        &[
            "Hana Kobayashi",
            "Olivia Bennett",
            "Chloé Martin",
            "Lucas Ferreira",
        ],
    ),
    (
        "Building 14 neighbours",
        &[
            "Sra. Pérez",
            "Andrés Villalba",
            "Laura Giménez",
            "Kenji Watanabe",
            "Paula Cárdenas",
        ],
    ),
    (
        "Lisbon trip ✈️",
        &[
            "Valentina Rojas",
            "Camila Duarte",
            "Noah Fischer",
            "Emma Lindqvist",
        ],
    ),
    (
        "Book club",
        &[
            "Isabella Conti",
            "Priya Raman",
            "Aisha Rahman",
            "Gabriela Suárez",
        ],
    ),
    (
        "Platform on-call",
        &[
            "Daniel Okafor",
            "Ethan Brooks",
            "Priya Raman",
            "Kenji Watanabe",
        ],
    ),
    (
        "Uni friends",
        &[
            "Mateo Salazar",
            "Camila Duarte",
            "Lucas Ferreira",
            "Sofía Méndez",
            "Rafael Pinto",
        ],
    ),
];

const THEIR_LINES: &[&str] = &[
    "Hey! Are you around later?",
    "Just landed, the flight was delayed almost two hours",
    "Can you send me the address again?",
    "That works for me 👍",
    "I'll call you when I'm out of the meeting",
    "Did you see what happened last night?",
    "Haha that's exactly what I thought",
    "Running ten minutes late, sorry!",
    "The invoice is attached, let me know if anything looks off",
    "Let's do Thursday instead, Wednesday is packed",
    "Thanks a lot, that really helped",
    "I left the keys with the doorman",
    "Do we need to bring anything?",
    "OK",
    "Sounds good",
    "Honestly I'm not sure yet. I need to check with the team first and see whether the budget covers it, but I'd say we have a good chance if we keep the scope small.",
    "Call me when you can",
    "Happy birthday!! 🎉🎂",
    "Where are you?",
    "The dentist moved my appointment to Friday at 9",
    "I just read it. It's good, but the second section needs a clearer example.",
    "No worries at all",
    "Dinner at ours on Saturday?",
    "Did the package arrive?",
    "Perfect, see you there",
    "Could you review my PR when you have a minute?",
    "I'm at the entrance",
    "It's raining like crazy here",
    "Let me know when you're home",
    "😂😂😂",
];

const MY_LINES: &[&str] = &[
    "On my way",
    "Sure, give me five minutes",
    "I can do after 6",
    "Sent it to your email too",
    "Yes! Just saw it",
    "Let me check and get back to you",
    "Haha no way",
    "Sorry, I was driving",
    "Sounds like a plan",
    "I'll bring dessert",
    "Thanks!",
    "Can we move it to tomorrow? Today got complicated",
    "Great, booking it now",
    "I think the second option is better. It's cheaper and we don't depend on the venue's schedule, which was the part that worried me the most.",
    "Did you get home OK?",
    "Almost there",
    "👍",
    "Not yet, tracking says tomorrow",
    "Call you in ten",
    "Deal",
];

const GROUP_LINES: &[&str] = &[
    "Who's in for Saturday?",
    "I can't this week, next one for sure",
    "Count me in",
    "Someone left the garage door open again",
    "Does anyone have a ladder I could borrow?",
    "Moving the sync to 10:30, there's a clash with the all-hands",
    "New mockups are in the shared folder",
    "Pitch is booked from 7 to 8",
    "I'll bring the balls and bibs",
    "Happy birthday abuela!! ❤️",
    "Photos from Sunday are amazing",
    "Flights are cheaper if we leave on the 14th",
    "I voted for the Airbnb near the river",
    "We're reading chapters 5 to 9 for next time",
    "Pager went off at 3am, it was the cache again",
    "Deploy is done, everything green",
    "Lunch at the usual place?",
    "+1",
    "Can't make it, sorry!",
    "Who has the receipt?",
    "Reminder: water will be shut off tomorrow 9-12",
    "lol",
    "Agreed",
    "Let's decide by Friday",
];

const REACTIONS: &[&str] = &["👍", "❤️", "😂", "😮", "🙏", "🔥"];

/// Everything the mock knows about one account.
pub(crate) struct World {
    pub(crate) accounts: Vec<Account>,
    pub(crate) chats: Vec<Chat>,
    /// Messages per chat, oldest first.
    pub(crate) messages: HashMap<(AccountId, ChatId), Vec<Message>>,
    /// Group members by chat, for live events.
    pub(crate) members: HashMap<(AccountId, ChatId), Vec<String>>,
    pub(crate) next_message: u64,
}

pub(crate) fn self_contact(account: &AccountId) -> ContactId {
    ContactId::new(format!("me:{account}"))
}

pub(crate) fn contact_for(name: &str) -> ContactId {
    let slug: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    ContactId::new(format!("contact:{slug}"))
}

pub(crate) fn their_line(rng: &mut Rng) -> &'static str {
    rng.pick(THEIR_LINES)
}

pub(crate) fn group_line(rng: &mut Rng) -> &'static str {
    rng.pick(GROUP_LINES)
}

fn preview_of(content: &MessageContent) -> String {
    match content {
        MessageContent::Text { body } => body.chars().take(80).collect(),
        MessageContent::Media(m) => match m.kind {
            MediaKind::Image => "Photo".into(),
            MediaKind::Video => "Video".into(),
            MediaKind::Voice => "Voice message".into(),
            MediaKind::Audio => "Audio".into(),
            MediaKind::Document => m.file_name.clone().unwrap_or_else(|| "Document".into()),
            MediaKind::Sticker => "Sticker".into(),
        },
        MessageContent::Reaction { emoji, .. } => emoji.clone(),
        MessageContent::Location(_) => "Location".into(),
        MessageContent::Contacts { .. } => "Contact".into(),
        MessageContent::Poll(poll) => format!("Poll · {}", poll.question),
        MessageContent::Event(event) => format!("Event · {}", event.title),
        MessageContent::System(_) | MessageContent::Unsupported { .. } => "Message".into(),
    }
}

fn media_placeholder(rng: &mut Rng, id: u64) -> Media {
    let mut media = match rng.below(4) {
        0 | 1 => {
            let mut m = Media::new(MediaKind::Image);
            m.mime_type = Some("image/jpeg".into());
            m.width = Some(1280);
            m.height = Some(*rng.pick(&[720, 960, 1600]));
            if rng.chance(40) {
                m.caption = Some(
                    (*rng.pick(&["Look at this view", "From yesterday", "The new place!"])).into(),
                );
            }
            m
        }
        2 => {
            let mut m = Media::new(MediaKind::Voice);
            m.mime_type = Some("audio/ogg; codecs=opus".into());
            m.duration_secs = Some(rng.range(3, 95) as u32);
            m
        }
        _ => {
            let mut m = Media::new(MediaKind::Document);
            m.mime_type = Some("application/pdf".into());
            m.file_name = Some(
                (*rng.pick(&[
                    "Invoice-0924.pdf",
                    "Itinerary.pdf",
                    "Q3 report.pdf",
                    "Lease.pdf",
                ]))
                .into(),
            );
            m.size_bytes = Some(rng.range(80_000, 4_200_000));
            m
        }
    };
    media.source = Some(MediaRef::new(
        match (media.kind, media.width, media.height) {
            // Pictures are drawn on request (see `picture`), at a size that
            // keeps their proportions.
            (MediaKind::Image, Some(width), Some(height)) => {
                format!("mock://image/{id}/{}x{}", width / 4, height / 4)
            }
            _ => format!("mock://media/{id}"),
        },
    ));
    media
}

/// Builds the seeded world. `now` anchors the timeline so the newest
/// messages are always "a moment ago".
pub(crate) fn build(seed: u64, now: Timestamp) -> World {
    let mut rng = Rng::new(seed);
    let accounts = vec![
        Account {
            id: AccountId::new("acc_personal"),
            display_name: "Personal".into(),
            phone: Some("+584245550199".into()),
            self_contact: Some(self_contact(&AccountId::new("acc_personal"))),
            connection: ConnectionState::Connected,
            settings: client_provider::AccountSettings {
                history_import: Some(client_provider::HistoryImport::Off),
                server_media: Some(client_provider::ServerMedia::OnDemand),
                ever_linked: Some(true),
            },
        },
        Account {
            id: AccountId::new("acc_work"),
            display_name: "Work".into(),
            phone: Some("+14155550142".into()),
            self_contact: Some(self_contact(&AccountId::new("acc_work"))),
            connection: ConnectionState::Connected,
            settings: client_provider::AccountSettings {
                history_import: Some(client_provider::HistoryImport::Recent),
                server_media: Some(client_provider::ServerMedia::Everything),
                ever_linked: Some(true),
            },
        },
    ];

    let mut world = World {
        accounts,
        chats: Vec::new(),
        messages: HashMap::new(),
        members: HashMap::new(),
        next_message: 1,
    };

    // (account, people range, groups range)
    let plan: [(&str, std::ops::Range<usize>, std::ops::Range<usize>); 2] =
        [("acc_personal", 0..22, 0..6), ("acc_work", 20..28, 6..8)];

    for (account, people, groups) in plan {
        let account = AccountId::new(account);
        let mut specs: Vec<(ChatKind, String, Vec<String>)> = Vec::new();
        for name in &PEOPLE[people] {
            specs.push((
                ChatKind::Direct,
                (*name).to_owned(),
                vec![(*name).to_owned()],
            ));
        }
        for (title, members) in &GROUPS[groups] {
            specs.push((
                ChatKind::Group,
                (*title).to_owned(),
                members.iter().map(|m| (*m).to_owned()).collect(),
            ));
        }
        // Interleave groups among the direct chats.
        let len = specs.len();
        for i in (1..len).rev() {
            specs.swap(i, rng.below(i as u64 + 1) as usize);
        }

        for (index, (kind, title, members)) in specs.into_iter().enumerate() {
            let chat_id = match kind {
                ChatKind::Direct => ChatId::new(contact_for(&title).into_string()),
                ChatKind::Group => ChatId::new(format!(
                    "group:{}",
                    contact_for(&title).as_str().trim_start_matches("contact:")
                )),
            };
            // The first chat is a very long thread to exercise virtualisation.
            let count = match index {
                0 => 640,
                1 | 4 => rng.range(140, 220),
                _ => rng.range(6, 48),
            } as usize;
            // Chats further down the list were last active longer ago.
            let minutes_ago = (index as f64).powf(2.1) * 7.0 + rng.below(4) as f64;
            let last_at = now.as_millis() - (minutes_ago * 60_000.0) as i64;
            let unread = if index > 0 && index < 9 && rng.chance(55) {
                rng.range(1, 7) as usize
            } else {
                0
            };
            let thread = build_thread(
                &mut world.next_message,
                &mut rng,
                &account,
                &chat_id,
                kind,
                &members,
                count,
                last_at,
                unread,
            );
            let last_message = thread
                .iter()
                .rev()
                .find(|m| !matches!(m.content, MessageContent::Reaction { .. }))
                .cloned();
            world.chats.push(Chat {
                id: chat_id.clone(),
                account_id: account.clone(),
                kind,
                title,
                avatar: None,
                unread_count: unread as u32,
                pinned: index == 2 || index == 6,
                muted: kind == ChatKind::Group && rng.chance(30),
                archived: false,
                last_message,
                unknown: Default::default(),
                picture_id: None,
                pinned_at: None,
            });
            world
                .members
                .insert((account.clone(), chat_id.clone()), members);
            world.messages.insert((account.clone(), chat_id), thread);
        }
    }
    crate::showcase::add(&mut world);
    crate::community::add(&mut world, now);
    world
}

#[allow(clippy::too_many_arguments)]
fn build_thread(
    next_id: &mut u64,
    rng: &mut Rng,
    account: &AccountId,
    chat: &ChatId,
    kind: ChatKind,
    members: &[String],
    count: usize,
    last_at: i64,
    unread: usize,
) -> Vec<Message> {
    // Walk backwards from the last message to place timestamps, then build
    // forwards so replies and reactions can point at earlier messages.
    let mut times = Vec::with_capacity(count);
    let mut t = last_at;
    for _ in 0..count {
        times.push(t);
        let gap_minutes = if rng.chance(70) {
            rng.range(1, 6)
        } else if rng.chance(70) {
            rng.range(20, 240)
        } else {
            rng.range(600, 2600)
        };
        t -= gap_minutes as i64 * 60_000 + rng.below(50_000) as i64;
    }
    times.reverse();

    let me = self_contact(account);
    let mut out: Vec<Message> = Vec::with_capacity(count + count / 6);
    let mut outgoing_streak = rng.chance(50);
    for (i, ts) in times.iter().enumerate() {
        let remaining = count - i;
        // The unread tail is always incoming.
        let outgoing = if remaining <= unread {
            false
        } else {
            if rng.chance(45) {
                outgoing_streak = !outgoing_streak;
            }
            outgoing_streak
        };
        let id = *next_id;
        *next_id += 1;

        let (sender, sender_name) = if outgoing {
            (me.clone(), None)
        } else {
            let name = rng.pick(members);
            (contact_for(name), Some(name.clone()))
        };

        let content = if rng.chance(8) {
            MessageContent::Media(media_placeholder(rng, id))
        } else if rng.chance(1) {
            MessageContent::Poll(crate::showcase::any_poll())
        } else {
            let line = if outgoing {
                *rng.pick(MY_LINES)
            } else if kind == ChatKind::Group {
                *rng.pick(GROUP_LINES)
            } else {
                *rng.pick(THEIR_LINES)
            };
            MessageContent::text(line)
        };

        let reply_to = if i > 2 && rng.chance(9) {
            let back = rng.range(1, 6.min(out.len() as u64)) as usize;
            out.iter()
                .rev()
                .filter(|m| !matches!(m.content, MessageContent::Reaction { .. }))
                .nth(back - 1)
                .map(|quoted| ReplyRef {
                    message_id: quoted.id.clone(),
                    sender_name: match quoted.direction {
                        Direction::Outgoing => None,
                        Direction::Incoming => quoted.sender_name.clone(),
                    },
                    preview: Some(preview_of(&quoted.content)),
                })
        } else {
            None
        };

        let status = if !outgoing {
            if remaining <= unread {
                DeliveryStatus::Delivered
            } else {
                DeliveryStatus::Read
            }
        } else if remaining == 1 {
            DeliveryStatus::Sent
        } else if remaining <= 3 {
            DeliveryStatus::Delivered
        } else if remaining == 9 && rng.chance(30) {
            DeliveryStatus::Failed {
                reason: "The recipient is not on WhatsApp".into(),
            }
        } else {
            DeliveryStatus::Read
        };

        let message_id = MessageId::new(format!("m{id}"));
        out.push(Message {
            id: message_id.clone(),
            client_id: None,
            account_id: account.clone(),
            chat_id: chat.clone(),
            sender,
            sender_name,
            direction: if outgoing {
                Direction::Outgoing
            } else {
                Direction::Incoming
            },
            timestamp: Timestamp::from_millis(*ts),
            content,
            reply_to,
            status,
            edited: rng.chance(2),
            deleted: false,
            extras: Default::default(),
        });

        // Now and then somebody reacts to the message just written.
        if rng.chance(11) && remaining > unread {
            let reactors = if kind == ChatKind::Group {
                rng.range(1, 3)
            } else {
                1
            };
            for r in 0..reactors {
                let rid = *next_id;
                *next_id += 1;
                let from_me = !outgoing && r == 0 && rng.chance(40);
                let (rs, rn) = if from_me {
                    (me.clone(), None)
                } else {
                    let name = &members[(r as usize + i) % members.len()];
                    (contact_for(name), Some(name.clone()))
                };
                out.push(Message {
                    id: MessageId::new(format!("m{rid}")),
                    client_id: None,
                    account_id: account.clone(),
                    chat_id: chat.clone(),
                    sender: rs,
                    sender_name: rn,
                    direction: if from_me {
                        Direction::Outgoing
                    } else {
                        Direction::Incoming
                    },
                    timestamp: Timestamp::from_millis(*ts + 20_000 + r as i64 * 7_000),
                    content: MessageContent::Reaction {
                        target: message_id.clone(),
                        emoji: (*rng.pick(REACTIONS)).to_owned(),
                    },
                    reply_to: None,
                    status: DeliveryStatus::Read,
                    edited: false,
                    deleted: false,
                    extras: Default::default(),
                });
            }
        }
    }
    out
}

/// How many frames a demo sticker has, and how long each stays, in
/// hundredths of a second.
const STICKER_FRAMES: u32 = 8;
const STICKER_DELAY: u16 = 9;

/// An animated sticker for a `mock://sticker/<id>/<side>` reference: a
/// GIF of a disc that swells and shrinks on a transparent ground, in a
/// colour that depends on the id. Written by hand, like the pictures, so
/// the mock needs no image library: the pixels are stored as LZW codes
/// that never grow past a byte, with the table reset before it would.
pub(crate) fn sticker(reference: &str) -> Option<Vec<u8>> {
    let rest = reference.strip_prefix("mock://sticker/")?;
    let (id, side) = rest.split_once('/')?;
    let side: u16 = side.parse().ok()?;
    if !(16..=512).contains(&side) {
        return None;
    }
    let hue = id
        .bytes()
        .fold(23u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let colour = [
        80 + (hue % 150) as u8,
        80 + ((hue >> 8) % 150) as u8,
        80 + ((hue >> 16) % 150) as u8,
    ];

    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&side.to_le_bytes());
    gif.extend_from_slice(&side.to_le_bytes());
    // A global table of 128 colours; 0 is the transparent ground.
    gif.extend_from_slice(&[0xf6, 0, 0]);
    let mut table = vec![0u8; 128 * 3];
    table[3..6].copy_from_slice(&colour);
    table[6..9].copy_from_slice(&[colour[0] / 2, colour[1] / 2, colour[2] / 2]);
    table[9..12].copy_from_slice(&[255, 255, 255]);
    gif.extend_from_slice(&table);
    // Loop for ever.
    gif.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");

    let centre = i32::from(side) / 2;
    for frame in 0..STICKER_FRAMES {
        // Out and back: 0, 1, 2, 3, 4, 3, 2, 1.
        let half = STICKER_FRAMES as i32 / 2;
        let step = half - (frame as i32 - half).abs();
        let radius = i32::from(side) / 4 + i32::from(side) * step / 20;
        // Graphic control: restore to the ground, transparent index 0.
        gif.extend_from_slice(&[0x21, 0xf9, 4, 0b0000_1001]);
        gif.extend_from_slice(&STICKER_DELAY.to_le_bytes());
        gif.extend_from_slice(&[0, 0]);
        // The image: the whole canvas, no table of its own.
        gif.push(0x2c);
        gif.extend_from_slice(&[0, 0, 0, 0]);
        gif.extend_from_slice(&side.to_le_bytes());
        gif.extend_from_slice(&side.to_le_bytes());
        gif.push(0);
        // Codes of 8 bits: 128 clears the table, 129 ends the data.
        gif.push(7);
        let mut codes = Vec::with_capacity(usize::from(side) * usize::from(side) * 102 / 100);
        let mut run = 0;
        for y in 0..i32::from(side) {
            for x in 0..i32::from(side) {
                if run == 0 {
                    codes.push(128);
                }
                run = (run + 1) % 100;
                let (dx, dy) = (x - centre, y - centre);
                let distance = dx * dx + dy * dy;
                codes.push(if distance > radius * radius {
                    0
                } else if distance > (radius - 4) * (radius - 4) {
                    2
                } else if dx * dx + (dy + radius / 3) * (dy + radius / 3) < radius * radius / 16 {
                    3
                } else {
                    1
                });
            }
        }
        codes.push(129);
        for block in codes.chunks(255) {
            gif.push(block.len() as u8);
            gif.extend_from_slice(block);
        }
        gif.push(0);
    }
    gif.push(0x3b);
    Some(gif)
}

/// A picture for a `mock://image/<id>/<width>x<height>` reference: a PNG
/// with a soft gradient whose colours depend on the id. Written by hand
/// (stored, uncompressed blocks) so the mock needs no image library.
pub(crate) fn picture(reference: &str) -> Option<Vec<u8>> {
    let rest = reference.strip_prefix("mock://image/")?;
    let (id, size) = rest.split_once('/')?;
    let (width, height) = size.split_once('x')?;
    let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
    if width == 0 || height == 0 || width > 2000 || height > 2000 {
        return None;
    }
    let hue = id
        .bytes()
        .fold(17u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let base = [
        (hue % 200) as u8,
        ((hue >> 8) % 200) as u8,
        ((hue >> 16) % 200) as u8,
    ];

    let mut raw = Vec::with_capacity((height * (1 + 3 * width)) as usize);
    for y in 0..height {
        raw.push(0); // no filter
        for x in 0..width {
            let light = ((x * 40 / width) + (y * 40 / height)) as u8;
            raw.extend_from_slice(&[
                base[0].saturating_add(light),
                base[1].saturating_add(light),
                base[2].saturating_add(light / 2),
            ]);
        }
    }
    // zlib: a header, stored deflate blocks, and the Adler-32 of the data.
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        zlib.push(u8::from(blocks.peek().is_none()));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc(&body).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bits, RGB
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    Some(png)
}
