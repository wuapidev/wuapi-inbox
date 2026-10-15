//! What a session hears, written to the archive and passed on as events.

use crate::archive::{StoredAccount, StoredContact, StoredMessage, StoredView};
use crate::convert::{self, DELIVERED, READ, SENT};
use crate::ids::{self, id_of, STATUS_CHAT};
use crate::mapping::{self, Effect};
use crate::provider::{qr_png, Inner};
use crate::session::{CallSignal, HistoryChat, Inbound, Incoming, ReceiptLevel};
use client_provider::{
    AccountId, CallEvent, CallState, ChatId, ConnectionState, ContactId, LinkStep, MessageContent,
    MessageId, ProviderEvent, StoryViewer, SystemEvent, SystemKind, Timestamp,
};
use std::sync::Arc;
use whatsapp_rust::Jid;

impl Inner {
    /// Takes one piece of news from the session kept under `device`.
    pub(crate) fn apply(self: &Arc<Self>, device: i32, inbound: Inbound) {
        let Some(account) = self.account_of_device(device) else {
            // The session spoke before its number was written down: keep
            // what it said until it is.
            self.early
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((device, inbound));
            return;
        };
        let id = AccountId::new(account.clone());
        match inbound {
            Inbound::Qr(code) => {
                let png = qr_png(&code);
                self.change_live(&account, |live| {
                    live.qr = Some(code);
                    // A code being typed stays on screen; the QR codes go
                    // on behind it.
                    if !matches!(live.step, LinkStep::TypeCode { .. }) {
                        if let Some(png) = png {
                            live.step = LinkStep::Scan { png };
                        }
                    }
                });
            }
            Inbound::Paired => self.change_live(&account, |live| live.step = LinkStep::Finishing),
            Inbound::LinkFailed(reason) => {
                self.change_live(&account, |live| live.step = LinkStep::Stopped { reason });
            }
            Inbound::Connected(identity) => {
                let row = self.archive.account(&account).ok().flatten();
                let mut row = row.unwrap_or_else(|| StoredAccount {
                    id: account.clone(),
                    device,
                    history: true,
                    ..StoredAccount::default()
                });
                row.phone = identity.pn.as_ref().map(id_of).or(row.phone);
                row.lid = identity.lid.as_ref().map(id_of).or(row.lid);
                if !identity.push_name.trim().is_empty() {
                    row.push_name = Some(identity.push_name.clone());
                }
                row.ever_linked = true;
                let _ = self.archive.put_account(&row);
                if let (Some(lid), Some(pn)) = (&row.lid, &row.phone) {
                    self.learn_alias(&account, lid, pn);
                }
                self.connection(&account, ConnectionState::Connected, Some(LinkStep::Linked));
            }
            Inbound::Reconnecting => {
                let linked = self
                    .archive
                    .account(&account)
                    .ok()
                    .flatten()
                    .is_some_and(|row| row.ever_linked);
                let gone = self
                    .live_of(&account)
                    .is_some_and(|live| live.connection == ConnectionState::LoggedOut);
                // While a number is being linked, the socket closing and
                // opening is part of linking, not a drop; and a number
                // that was logged out is not coming back by itself.
                if linked && !gone {
                    self.connection(&account, ConnectionState::Reconnecting, None);
                }
            }
            Inbound::Disconnected(reason) => {
                self.connection(&account, ConnectionState::Disconnected { reason }, None);
            }
            Inbound::LoggedOut(reason) => {
                let reason =
                    reason.unwrap_or_else(|| "This number was logged out on the phone.".into());
                self.connection(
                    &account,
                    ConnectionState::LoggedOut,
                    Some(LinkStep::Stopped { reason }),
                );
            }
            Inbound::Messages(batch) => {
                for incoming in batch {
                    self.take_message(&id, incoming, true);
                }
            }
            Inbound::History {
                chats,
                names,
                aliases,
                asked,
            } => {
                for (lid, pn) in &aliases {
                    self.learn_alias(&account, &id_of(lid), &id_of(pn));
                }
                for (jid, name) in names {
                    self.learn_name(&id, &jid, None, Some(name), false);
                }
                for chat in chats {
                    self.take_history(&id, chat);
                }
                if asked {
                    self.history_news.notify_waiters();
                }
            }
            Inbound::Receipt {
                chat,
                from,
                ids,
                level,
                timestamp_ms,
            } => self.take_receipt(&id, &chat, &from, &ids, level, timestamp_ms),
            Inbound::Pinned {
                chat,
                pinned,
                at_ms,
            } => self.change_chat(&id, &chat, |stored| {
                stored.pinned_at_ms = pinned.then_some(at_ms.max(0));
            }),
            Inbound::Muted { chat, until_ms } => {
                self.change_chat(&id, &chat, |stored| stored.muted_until_ms = until_ms);
            }
            Inbound::Archived { chat, archived } => {
                self.change_chat(&id, &chat, |stored| stored.archived = archived);
            }
            Inbound::MarkedRead { chat, read } => self.change_chat(&id, &chat, |stored| {
                stored.marked_unread = !read;
                if read {
                    stored.unread = 0;
                }
            }),
            Inbound::ChatDeleted { chat } => {
                let chat = self.canonical(&account, &chat, None);
                let _ = self.archive.remove_chat(&account, &chat);
            }
            Inbound::Starred {
                chat,
                id: message,
                starred,
            } => {
                let chat = self.canonical(&account, &chat, None);
                if matches!(
                    self.archive.set_starred(&account, &chat, &message, starred),
                    Ok(true)
                ) {
                    self.message_changed(&id, &chat, &message);
                }
            }
            Inbound::DeletedForMe { chat, id: message } => {
                // The model has no "gone": the message stays as one that
                // was deleted.
                let chat = self.canonical(&account, &chat, None);
                if matches!(
                    self.archive.mark_deleted(&account, &chat, &message),
                    Ok(true)
                ) {
                    self.message_changed(&id, &chat, &message);
                }
            }
            Inbound::Contact {
                jid,
                alt,
                saved_name,
                username,
            } => {
                let person = self.canonical(&account, &jid, alt.as_ref());
                if saved_name.is_none() {
                    let _ = self.archive.forget_saved_name(&account, &person);
                }
                self.learn(
                    &id,
                    StoredContact {
                        id: person,
                        saved_name,
                        username,
                        ..StoredContact::default()
                    },
                    true,
                );
            }
            Inbound::ProfileName { jid, name } => {
                self.learn_name(&id, &jid, None, Some(name), true)
            }
            Inbound::Activity { chat, from, state } => {
                self.emit(ProviderEvent::Presence {
                    account_id: id.clone(),
                    chat_id: ChatId::new(self.canonical(&account, &chat, None)),
                    contact_id: ContactId::new(self.canonical(&account, &from, None)),
                    state,
                });
            }
            Inbound::Picture { jid, picture_id } => {
                let subject = self.canonical(&account, &jid, None);
                if let Ok(Some(_)) = self.archive.chat(&account, &subject) {
                    self.change_chat(&id, &jid, |stored| stored.picture_id = picture_id.clone());
                }
            }
            Inbound::GroupChanged {
                group,
                notice,
                subject,
                community,
            } => {
                let group_id = id_of(&group);
                if let Some(subject) = subject {
                    self.change_chat(&id, &group, |stored| stored.name = Some(subject.clone()));
                }
                if let Some((message, at_ms, notice)) = notice {
                    self.keep_notice(&id, &group_id, &message, at_ms, &notice, false);
                }
                self.emit(ProviderEvent::GroupChanged {
                    account_id: id.clone(),
                    group_id: ChatId::new(group_id.clone()),
                });
                if let Some(community) = community {
                    self.emit(ProviderEvent::CommunityChanged {
                        account_id: id.clone(),
                        community_id: ids::chat_id(&community),
                        groups: vec![ChatId::new(group_id)],
                    });
                }
            }
            Inbound::PollVote {
                chat,
                poll,
                voter,
                mine,
                hashes,
                timestamp_ms,
            } => {
                let chat = self.canonical(&account, &chat, None);
                let voter = if mine {
                    self.own(&account)
                } else {
                    self.canonical(&account, &voter, None)
                };
                let _ = self.archive.put_vote(
                    &account,
                    &crate::archive::Vote {
                        chat: &chat,
                        poll: &poll,
                        voter: &voter,
                        mine,
                        hashes: &hashes,
                        timestamp_ms,
                    },
                );
                self.message_changed(&id, &chat, &poll);
            }
            Inbound::StoryMuted { author, muted } => {
                let author = self.canonical(&account, &author, None);
                let _ = self.archive.set_value(
                    &account,
                    &format!("story_mute:{author}"),
                    muted.then_some("1"),
                );
                self.emit(ProviderEvent::StoryMuteChanged {
                    account_id: id.clone(),
                    contact: ContactId::new(author),
                    muted,
                });
            }
            Inbound::StoryAudience(privacy) => {
                let _ = self.archive.set_value(
                    &account,
                    "story_privacy",
                    serde_json::to_string(&privacy).ok().as_deref(),
                );
            }
            Inbound::Call {
                id: call,
                peer,
                signal,
                timestamp_ms,
            } => self.take_call(&id, call, peer, signal, timestamp_ms),
        }
    }

    /// Replays what a session said before its number was written down.
    pub(crate) fn replay_early(self: &Arc<Self>, device: i32) {
        let early: Vec<_> = {
            let mut held = self
                .early
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let (now, later) = std::mem::take(&mut *held)
                .into_iter()
                .partition(|(number, _)| *number == device);
            *held = later;
            now
        };
        for (number, inbound) in early {
            self.apply(number, inbound);
        }
    }

    fn connection(&self, account: &str, state: ConnectionState, step: Option<LinkStep>) {
        let mut changed = false;
        self.change_live(account, |live| {
            changed = live.connection != state;
            live.connection = state.clone();
            if let Some(step) = step {
                live.step = step;
            }
        });
        if changed {
            self.emit(ProviderEvent::ConnectionChanged {
                account_id: AccountId::new(account),
                state,
            });
        }
    }

    fn own(&self, account: &str) -> String {
        self.archive
            .account(account)
            .ok()
            .flatten()
            .and_then(|row| row.phone.or(row.lid))
            .unwrap_or_else(|| account.to_owned())
    }

    fn change_chat(
        &self,
        account: &AccountId,
        chat: &Jid,
        change: impl FnOnce(&mut crate::archive::StoredChat),
    ) {
        let id = self.canonical(account.as_str(), chat, None);
        if let Ok(stored) = self.archive.update_chat(account.as_str(), &id, change) {
            self.chat_changed(account.as_str(), &stored);
        }
    }

    fn message_changed(&self, account: &AccountId, chat: &str, message: &str) {
        let Ok(Some(stored)) = self.archive.message(account.as_str(), chat, message) else {
            return;
        };
        if let Some(model) = self.message_model(account, &stored) {
            self.emit(ProviderEvent::MessageUpserted(model));
        }
    }

    fn learn_name(
        &self,
        account: &AccountId,
        jid: &Jid,
        alt: Option<&Jid>,
        profile_name: Option<String>,
        announce: bool,
    ) {
        let person = self.canonical(account.as_str(), jid, alt);
        self.learn(
            account,
            StoredContact {
                id: person,
                profile_name,
                ..StoredContact::default()
            },
            announce,
        );
    }

    /// Adds to what is known about a person and says so when it is news.
    fn learn(&self, account: &AccountId, learned: StoredContact, announce: bool) {
        let Ok((known, changed)) = self.archive.learn_contact(account.as_str(), &learned) else {
            return;
        };
        if !changed || !announce {
            return;
        }
        let alt = if known.id.starts_with('+') {
            self.archive
                .hidden_id_of(account.as_str(), &known.id)
                .ok()
                .flatten()
        } else {
            self.archive
                .number_of(account.as_str(), &known.id)
                .ok()
                .flatten()
        };
        self.emit(ProviderEvent::ContactUpdated(convert::to_contact(
            account, &known, alt,
        )));
        // The chat with them is named after them.
        if let Ok(Some(chat)) = self.archive.chat(account.as_str(), &known.id) {
            self.chat_changed(account.as_str(), &chat);
        }
    }

    /// One message: kept, and when `live` announced.
    fn take_message(&self, account: &AccountId, incoming: Incoming, live: bool) {
        let story = id_of(&incoming.chat) == STATUS_CHAT;
        if !story && !ids::is_conversation(&incoming.chat) {
            return;
        }
        let me = account.as_str();
        let chat = if story {
            STATUS_CHAT.to_owned()
        } else {
            self.canonical(me, &incoming.chat, incoming.chat_alt.as_ref())
        };
        let sender = if incoming.from_me {
            self.own(me)
        } else {
            self.canonical(me, &incoming.sender, incoming.sender_alt.as_ref())
        };
        if !incoming.from_me {
            if let Some(name) = incoming
                .push_name
                .clone()
                .filter(|name| !name.trim().is_empty())
            {
                self.learn(
                    account,
                    StoredContact {
                        id: sender.clone(),
                        profile_name: Some(name),
                        ..StoredContact::default()
                    },
                    live,
                );
            }
        }
        match mapping::effect(&incoming.message, None) {
            Effect::Content(mapped) => {
                let reaction = matches!(mapped.content, MessageContent::Reaction { .. });
                let stored = StoredMessage {
                    chat: chat.clone(),
                    id: incoming.id.clone(),
                    sender: sender.clone(),
                    sender_name: incoming
                        .push_name
                        .clone()
                        .filter(|name| !name.trim().is_empty()),
                    from_me: incoming.from_me,
                    timestamp_ms: incoming.timestamp_ms,
                    status: incoming.status.unwrap_or(if incoming.from_me {
                        SENT
                    } else {
                        DELIVERED
                    }),
                    starred: incoming.starred,
                    edited: false,
                    deleted: false,
                    client_id: None,
                    proto: convert::encode(&incoming.message),
                };
                if self.archive.put_message(me, &stored).is_err() || !live {
                    return;
                }
                if story {
                    if let Ok(Some(held)) = self.archive.message(me, &chat, &stored.id) {
                        if let Some(model) = self.story_model(account, &held) {
                            self.emit(ProviderEvent::StoryUpserted(model));
                        }
                    }
                    return;
                }
                let updated = self.archive.update_chat(me, &chat, |held| {
                    if !reaction {
                        held.last_ms = held.last_ms.max(stored.timestamp_ms);
                    }
                    if stored.from_me {
                        // Written on another of the account's devices:
                        // the chat is being read there.
                        held.unread = 0;
                        held.marked_unread = false;
                    } else if !reaction {
                        held.unread = held.unread.saturating_add(1);
                    }
                });
                self.message_changed(account, &chat, &stored.id);
                if let Ok(updated) = updated {
                    self.chat_changed(me, &updated);
                }
            }
            Effect::Revoke { target } => {
                if !matches!(self.archive.mark_deleted(me, &chat, &target), Ok(true)) || !live {
                    return;
                }
                if story {
                    self.emit(ProviderEvent::StoryRemoved {
                        account_id: account.clone(),
                        story_id: MessageId::new(target),
                    });
                } else {
                    self.message_changed(account, &chat, &target);
                }
            }
            Effect::Edit {
                target,
                replacement,
            } => {
                let replaced =
                    self.archive
                        .replace(me, &chat, &target, &convert::encode(&replacement));
                if matches!(replaced, Ok(true)) && live {
                    self.message_changed(account, &chat, &target);
                }
            }
            Effect::Pin {
                target,
                pin,
                seconds,
                at_ms,
            } => {
                let until = pin.then(|| {
                    let from = at_ms.unwrap_or(incoming.timestamp_ms);
                    let length = seconds.map_or_else(
                        || client_provider::PinFor::default().duration(),
                        |seconds| std::time::Duration::from_secs(u64::from(seconds)),
                    );
                    from + length.as_millis() as i64
                });
                if self.archive.set_pin(me, &chat, &target, until).is_ok() && live {
                    self.message_changed(account, &chat, &target);
                }
            }
            Effect::PollVote { poll } => {
                if self.take_vote(account, &chat, &poll, &incoming, &sender) && live {
                    self.message_changed(account, &chat, &poll);
                }
            }
            Effect::Nothing => {}
        }
    }

    /// The addresses a person may have been named by when a vote was
    /// encrypted: by their number and by their hidden id.
    fn wires(&self, account: &str, id: &str) -> Vec<String> {
        let other = if id.starts_with('+') {
            self.archive.hidden_id_of(account, id).ok().flatten()
        } else {
            self.archive.number_of(account, id).ok().flatten()
        };
        std::iter::once(id.to_owned())
            .chain(other)
            .filter_map(|id| ids::jid_of(&id))
            .map(|jid| convert::wire(&jid))
            .collect()
    }

    /// A vote in a poll this device holds: opened with the poll's secret
    /// and kept. Answers whether it could be read.
    fn take_vote(
        &self,
        account: &AccountId,
        chat: &str,
        poll: &str,
        incoming: &Incoming,
        voter: &str,
    ) -> bool {
        use whatsapp_rust::wacore::poll::{decrypt_poll_vote_with_secret, PollVoteCiphertext};
        let me = account.as_str();
        let Ok(Some(stored)) = self.archive.message(me, chat, poll) else {
            return false;
        };
        let secret = convert::decode(&stored.proto).and_then(|poll| convert::poll_secret(&poll));
        let Some(secret) = secret else {
            return false;
        };
        let base = mapping::unwrap(&incoming.message);
        let vote = base
            .poll_update_message
            .as_option()
            .and_then(|update| update.vote.as_option());
        let Some((payload, iv)) =
            vote.and_then(|vote| Some((vote.enc_payload.as_deref()?, vote.enc_iv.as_deref()?)))
        else {
            return false;
        };
        // A vote is encrypted for the poll's maker and its voter by the
        // addresses the voter's phone used, which may be either of each.
        let creators = self.wires(me, &stored.sender);
        let voters = self.wires(me, voter);
        let hashes = creators.iter().find_map(|creator| {
            voters.iter().find_map(|by| {
                let ciphertext = PollVoteCiphertext {
                    enc_payload: payload,
                    enc_iv: iv,
                };
                decrypt_poll_vote_with_secret(ciphertext, &secret, poll, creator, by).ok()
            })
        });
        let Some(hashes) = hashes else {
            tracing::debug!("a poll vote could not be opened");
            return false;
        };
        self.archive
            .put_vote(
                me,
                &crate::archive::Vote {
                    chat,
                    poll,
                    voter,
                    mine: incoming.from_me,
                    hashes: &hashes,
                    timestamp_ms: incoming.timestamp_ms,
                },
            )
            .is_ok()
    }

    /// A chat as the phone handed it over.
    fn take_history(&self, account: &AccountId, history: HistoryChat) {
        let me = account.as_str();
        let story = id_of(&history.chat) == STATUS_CHAT;
        if !story && !ids::is_conversation(&history.chat) {
            return;
        }
        let chat = if story {
            STATUS_CHAT.to_owned()
        } else {
            self.canonical(me, &history.chat, history.chat_alt.as_ref())
        };
        let mut newest = 0;
        for mut incoming in history.messages {
            newest = newest.max(incoming.timestamp_ms);
            // The messages of a chat that was handed over under one
            // address belong to it under the other too.
            incoming.chat = history.chat.clone();
            incoming.chat_alt = incoming.chat_alt.or_else(|| history.chat_alt.clone());
            self.take_message(account, incoming, false);
        }
        if story {
            return;
        }
        let updated = self.archive.update_chat(me, &chat, |held| {
            if let Some(name) = history.name.clone().filter(|name| !name.trim().is_empty()) {
                held.name = Some(name);
            }
            if let Some(unread) = history.unread {
                held.unread = unread;
            }
            held.marked_unread = held.marked_unread || history.marked_unread;
            held.archived = held.archived || history.archived;
            if history.pinned_at_ms.is_some() {
                held.pinned_at_ms = history.pinned_at_ms;
            }
            if history.muted_until_ms.is_some() {
                held.muted_until_ms = history.muted_until_ms;
            }
            held.last_ms = held.last_ms.max(newest);
        });
        if let Ok(updated) = updated {
            self.chat_changed(me, &updated);
        }
    }

    fn take_receipt(
        &self,
        account: &AccountId,
        chat: &Jid,
        from: &Jid,
        ids: &[String],
        level: ReceiptLevel,
        timestamp_ms: i64,
    ) {
        let me = account.as_str();
        if id_of(chat) == STATUS_CHAT {
            // Somebody saw a story of the account.
            if level != ReceiptLevel::Read {
                return;
            }
            let viewer = self.canonical(me, from, None);
            for story in ids {
                let mine = self
                    .archive
                    .message(me, STATUS_CHAT, story)
                    .ok()
                    .flatten()
                    .is_some_and(|stored| stored.from_me);
                if !mine {
                    continue;
                }
                let view = StoredView {
                    viewer: viewer.clone(),
                    at_ms: timestamp_ms,
                    reaction: None,
                };
                let _ = self.archive.put_view(me, story, &view);
                self.emit(ProviderEvent::StoryViewed {
                    account_id: account.clone(),
                    story_id: MessageId::new(story.clone()),
                    viewer: StoryViewer {
                        contact: ContactId::new(viewer.clone()),
                        name: None,
                        viewed_at: Timestamp::from_millis(timestamp_ms),
                        reaction: None,
                    },
                });
            }
            return;
        }
        let chat = self.canonical(me, chat, None);
        let rank = Self::delivered_rank(level == ReceiptLevel::Read);
        for message in ids {
            let sent = self
                .archive
                .message(me, &chat, message)
                .ok()
                .flatten()
                .is_some_and(|stored| stored.from_me);
            if sent && matches!(self.archive.advance(me, &chat, message, rank), Ok(true)) {
                self.emit(ProviderEvent::MessageStatusChanged {
                    account_id: account.clone(),
                    chat_id: ChatId::new(chat.clone()),
                    message_id: MessageId::new(message.clone()),
                    status: convert::status_of(rank),
                });
            }
        }
    }

    /// Writes a line a conversation shows that no message carried, and
    /// says so. `counts` makes it an unread message of the chat.
    fn keep_notice(
        &self,
        account: &AccountId,
        chat: &str,
        id: &str,
        at_ms: i64,
        notice: &SystemEvent,
        counts: bool,
    ) {
        let me = account.as_str();
        let stored = StoredMessage {
            chat: chat.to_owned(),
            id: id.to_owned(),
            sender: notice
                .actor
                .as_ref()
                .map(|actor| actor.id.to_string())
                .unwrap_or_else(|| chat.to_owned()),
            sender_name: None,
            from_me: false,
            timestamp_ms: at_ms,
            status: READ,
            starred: false,
            edited: false,
            deleted: false,
            client_id: None,
            proto: convert::encode_notice(notice),
        };
        if self.archive.put_message(me, &stored).is_err() {
            return;
        }
        let updated = self.archive.update_chat(me, chat, |held| {
            held.last_ms = held.last_ms.max(at_ms);
            if counts {
                held.unread = held.unread.saturating_add(1);
            }
        });
        self.message_changed(account, chat, id);
        if let Ok(updated) = updated {
            self.chat_changed(me, &updated);
        }
    }

    fn take_call(
        &self,
        account: &AccountId,
        call: String,
        peer: Jid,
        signal: CallSignal,
        timestamp_ms: i64,
    ) {
        let me = account.as_str();
        let chat = self.canonical(me, &peer, None);
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let video = calls.get(&call).map(|held| held.video);
        // What the call leaves in the chat when it ends, if this is where
        // it ends.
        let mut ended: Option<crate::provider::HeldCall> = None;
        let state = match &signal {
            CallSignal::Ringing { video, group } => {
                calls.insert(
                    call.clone(),
                    crate::provider::HeldCall {
                        video: *video,
                        answered: false,
                        incoming: true,
                        connected_ms: None,
                    },
                );
                CallState::Ringing {
                    incoming: true,
                    group: *group,
                }
            }
            CallSignal::Connected => {
                if let Some(held) = calls.get_mut(&call) {
                    held.answered = true;
                    held.connected_ms.get_or_insert(timestamp_ms);
                }
                CallState::Connected
            }
            CallSignal::Ended { missed, reason } => {
                let held = calls.remove(&call);
                let missed = *missed && !held.as_ref().is_some_and(|held| held.answered);
                ended = held;
                CallState::Ended {
                    missed,
                    reason: reason.clone(),
                }
            }
        };
        drop(calls);
        let video = match &signal {
            CallSignal::Ringing { video, .. } => *video,
            _ => video.unwrap_or(false),
        };
        if let CallState::Ended { missed: true, .. } = state {
            let notice = SystemEvent {
                kind: if video {
                    SystemKind::MissedVideoCall
                } else {
                    SystemKind::MissedVoiceCall
                },
                actor: None,
                targets: Vec::new(),
                detail: None,
            };
            self.keep_notice(
                account,
                &chat,
                &convert::call_notice_id(&call),
                timestamp_ms,
                &notice,
                true,
            );
        } else if let Some(held) = ended {
            // Every other call leaves its line too: which way it went and
            // how it ended. It moves the chat up, and counts as nothing
            // to read.
            let notice = SystemEvent {
                kind: if video {
                    SystemKind::VideoCall
                } else {
                    SystemKind::VoiceCall
                },
                actor: None,
                targets: Vec::new(),
                detail: Some(call_outcome(&held, timestamp_ms)),
            };
            self.keep_notice(
                account,
                &chat,
                &convert::call_notice_id(&call),
                timestamp_ms,
                &notice,
                false,
            );
        }
        self.emit(ProviderEvent::Call(CallEvent {
            account_id: account.clone(),
            call_id: call,
            chat_id: ChatId::new(chat.clone()),
            peer: ContactId::new(chat),
            video,
            state,
            at: Timestamp::from_millis(timestamp_ms),
        }));
    }
}

/// How a call that is over went, for its line in the chat: which way,
/// and how long it lasted or why it did not happen.
fn call_outcome(call: &crate::provider::HeldCall, ended_ms: i64) -> String {
    let way = if call.incoming {
        "Incoming"
    } else {
        "Outgoing"
    };
    let how = match (call.connected_ms, call.incoming) {
        (Some(since), _) => {
            let seconds = (ended_ms - since).max(0) / 1000;
            format!("{}:{:02}", seconds / 60, seconds % 60)
        }
        (None, true) => "declined".to_owned(),
        (None, false) => "no answer".to_owned(),
    };
    format!("{way} · {how}")
}
