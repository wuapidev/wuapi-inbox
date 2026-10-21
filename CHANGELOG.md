# Changelog

## [Unreleased]

* Scrolling up a long chat no longer freezes the window. Reaching the top shows the window of older messages before it and the view stays on the message it was on; it jumped to the top of what was added, which asked for more again, until the whole history was read, once per wheel event.
* The search in a conversation is asked off the interface's thread, a moment after typing stops, and only in that chat; going to a match reads the history down to it, not the whole chat.
* The chat list's search reads the chats once per letter, not three times.
* A large file no longer stalls the window. A bubble of a document, a video or a voice note read its whole file from the database each time it was drawn, to know whether it was there; while a file was being sent, the conversation was read again several times a second for its progress; and the interface waited for the engine's reads, such as the file being read to upload it. The interface now has its own connection to the database, and the first page of a PDF is read off its thread too.
* The open chat's messages and the chat list are read off the interface's thread: a long history or a long list no longer holds up the window while it is read, at every message that arrives. A chat opens with its newest messages at once and the rest of what it shows follows; what changes during a read is read after it. The chat list asks who each person is once per read, not once per chat.
* `Store::has_media`, `Store::search_chat_messages`, `Store::messages_since`, `Store::has_communities` and `Store::reserve_reader`; `StoreChange::Upload`: how far a file going up is, with nothing stored changed.

## [0.5.3]

* A PDF in a chat shows the top of its first page over its name, and how many pages it has. Before its file is here it is the small picture the message brings; once it is here (with the numbers of this computer it downloads by itself), its own first page is drawn sharp and kept. A click on it reads it in the window.
* A PDF sent from here goes with the top of its first page and how many pages it has, so the other side sees it before downloading it, as with one sent from the phone. With the numbers of this computer; Wuapi Cloud's API takes no picture for a file. A PDF forwarded keeps the picture it came with.
* A voice note that is not here yet has two buttons: its own plays it (downloading it first), and the one beside the waveform only downloads it, with a ring that stops it while it comes. The one button did both, so downloading the next note while one was heard stopped that one and played the next.
* `OutgoingContent::Media` has `preview` (`FilePreview`): the picture the other side shows of a file before downloading it. `PreparedMedia::with_preview` sets it.
* `Media` has `pages`: how many pages a document has, when the message says.

## [0.5.2]

* A picture, a video or a document whose caption was edited keeps its file. The edit carries the new caption and not the file again, and it replaced the whole message, so the picture showed as "Not available · From before this number was linked". Ones edited before this version stay so.
* Media downloads by itself as it comes into view, videos, audio and documents included, with the numbers of this computer: "Automatic", the new default of Settings > Download media automatically, is everything for them and images, stickers and GIFs through Wuapi Cloud. Images, Never and Everything are still there; a choice of Never or Everything made before this version is kept.
* Link a number outlines only the place chosen: Wuapi Cloud, the recommended one, was outlined too and read as chosen beside This computer.
* A quote names who the quoted message mentions ("@Ana"), as the message itself does, instead of the digits of their id. The text that comes with an answer is the one typed on the phone; the quote takes the local copy of the message, or names the people it lists when there is none.
* `ReplyRef` has `mentions`: who the quoted text mentions.
* A message is reacted to from beside its bubble: a face shows next to it while the pointer is on the row (on the right of what others write, on the left of one's own), and a click opens the six reactions and the rest right there, over it.
* WhatsApp links open here: `wa.me/<number>`, `whatsapp://send`, and `api.whatsapp.com/send` and `web.whatsapp.com/send`. The chat with that number opens, with the link's text in the composer, ready to send and never sent by itself. A link with a text and no number opens New chat, and the text goes to the person chosen there. A link that comes before there is a number to write from waits until there is one. A group's invite link is said to be not followed: joining from a link cannot be done here yet.
* Settings > About says what opens WhatsApp links on this computer, and "Open WhatsApp links with Wuapi" makes it this application; the palette has the same. On Linux the desktop entry is put right first, for an entry installed before this version. On Windows a choice made in Windows' settings is changed there, and they are opened for it. On macOS the system may ask to confirm.
* A WhatsApp link in a message opens here instead of in the browser.
* One window at a time: a second start, by hand or from a link, brings the window already open to the front and hands it its link, instead of opening a second copy over the same data.
* `wuapi-inbox <link>` opens a WhatsApp link, and the desktop entry and the macOS bundle declare the `whatsapp:` scheme.

## [0.5.1]

* A video plays with its sound. The player asked for a sound output that does not exist and fell back to none, so every video (a chat's and a story's) played silent.
* The player has a button for the sound, and M: a video muted stays so for the next one, until it is turned on again.
* A video of a chat shows as a video: its first frame, the play button and how long it is, and a click plays it in the window (it was a card of a file, with "Download and open"). Save as… is on its corner. The first frame comes in the message, so videos received before this version show the play button on black.
* A sticker sent as an answer shows the message it answers above it, as a bubble does.
* A chat's gallery shows its pictures: a page of it asked for more at once than the downloads waiting could hold, so they pushed each other out and stayed "Loading…". A page is 60 now.
* `Media` has `thumbnail`: a small picture of a video, as the message carries it.

## [0.5.0]

* Tasks. A task is written in the tasks pane, or made from a message: "Create task" in the message's menu, or T, opens it with the message as its title. Each task has notes and a day (today, tomorrow, next week, or any day on a calendar). They are grouped as late, today, coming up and no date, with the done ones folded at the end, and "This chat" shows only the open chat's. A task made from a message shows its chat and goes back to the message, and that message has a mark while the task is to do.
* The tasks pane stands beside the conversation, so a chat and the tasks are read together, and stays open from chat to chat until it is closed. It opens from its button on the rail, which counts the tasks due today or late, or with Alt+T; "New task" is in the palette. In a narrow window it lies over the conversation instead of squeezing it. Tasks are kept in the encrypted store of this computer and leave with the number their message came to; WhatsApp has no tasks, so nothing of them is sent.
* A message is pinned at the top of its chat for everyone in it, for 24 hours, 7 days or 30 days: "Pin" in its menu, or P, and the same to unpin. A bar under the chat's header shows the pinned message and goes to it; a chat keeps three, and one more replaces the one that ends first. Pins made on the phone or by others in the chat show too. With the numbers of this computer; Wuapi Cloud's API cannot pin yet, so the entry is not there.
* Videos play inside the window, everywhere: a story's video, a chat's video and a GIF. Releases did not include the video player before, so story videos could not be played in any of them (on macOS too).
* A video of a chat opens in a player in the window: Space plays and holds, the bar goes anywhere in it, the arrows go five seconds back and forward, with Save as… and the way to open it with another application.
* A GIF plays in its bubble, without sound and over and over, once it is downloaded (it was a file to open). With reduced motion it stays a tile.
* Albums are one bubble: the pictures and videos sent as an album show as a grid (two side by side, three as two over one, four or more as two rows of two with how many more over the last), however few they are and however long they took to arrive. The message that opens an album no longer shows as a message this version cannot show. A click opens a picture in the viewer, whose arrows go through the rest. With the numbers of this computer, which WhatsApp tells which album a picture is in; with Wuapi Cloud, whose API does not say, four or more pictures the same person sent one after the other, each within a minute of the one before, are taken for an album.
* A chat's photos and videos, files and links are listed from its panel, for a person and for a group ("Media, files and links"): by month, newest first, with who sent each and when, and the way to the message in the conversation.
* A video whose sound cannot play (no sound card, or one in use) plays without sound instead of saying it cannot be played.
* A chat's picture is no longer downloaded again at every start when the chat list names it differently from the picture itself.
* The chats on this computer move to a newer format when 0.5.0 first opens them. 0.4.0 cannot open them after that, so going back to it is not done by itself.
* The provider trait has `pin_message`, the `pins` capability, `MessageExtras::pinned_until`, `PinFor`, and `MessageExtras::album` (the album a picture was sent in).
* Releases build their video player on each platform, from pinned sources: libmpv with FFmpeg, LGPL only (`packaging/video/build-libmpv.sh`, `video-runtime.yml`). A release without it fails. On macOS it is signed with the application's Developer ID.
* Releases try stapling the notarization ticket on macOS a few times before failing (`packaging/macos/staple.sh`).

## [0.4.0]

* A PDF somebody sent is read inside the window: its pages one under the other, scrolled with the wheel, the arrows, Space and the page keys, with the page being read and their number in the toolbar, zoom, Save as… and the way to open it with another application. A PDF that asks for a password, and any other kind of file, still opens with the system's application.
* The list of people behind `@`: Up and Down move in it and Tab picks (the text field kept those keys to itself), and each person is shown with their picture.
* Copying or saving the picture in the viewer says so over the picture. It was said above the chat list, behind the viewer.
* A picture is retouched before it is sent: the sheet of files opens large on a picture, with tools over it to turn it, crop it, draw on it, put a rectangle around something, hide a part (turned into blocks) and write words on it, in a few colours, and Undo. What is sent is the retouched picture; with every edit taken back, the file as it came.
* Far up a conversation, a button in its corner goes back to the newest message.
* A message somebody wrote in a group is answered in private: "Reply privately" in its menu (Shift+R) opens the chat with them, and what is sent there quotes the message of the group. A click on that quote goes back to the group. With Wuapi Cloud and with the numbers of this computer.
* The provider trait has the `private_replies` capability: `OutgoingMessage::reply_to` may then name a message of a group, sent to the chat with whoever wrote it.
* A group's "Manage" page is laid out as cards: what the group is (name, description, picture), its invite link, and what participants may do. A question asked on that page stays inside its card.
* New dependencies: `hayro` (draws the pages of a PDF) and `ab_glyph` (the words written on a picture).

## [0.3.0]

* A provider with no API behind it: `provider-native` speaks WhatsApp's own protocol from this computer, through whatsapp-rust. Each number is linked to the phone like WhatsApp Web, by QR code or by a code typed on the phone, and its keys are kept encrypted on this computer. In a build with the `provider-native` feature: `--provider native`. See "How the native adapter is built" and its known gaps in `docs/PROVIDERS.md`.
* Calls, where the provider carries them (the native provider: voice, one to one). A call that comes in shows a card with Answer and Decline, a person's chat has a call button, and a call nobody answered leaves its line in the chat.
* Calls with video on the native provider, where `ffmpeg` is installed: a video button beside the call button, the other side's picture with the own camera in its corner, and a switch for the camera. Without `ffmpeg`, calls are voice only.
* Calls use the microphone and speakers chosen in Settings > Audio and video, which also chooses the camera. A call's card shows how long it has been on.
* A call rings: a chord that repeats when somebody calls, a soft tone while a call the account placed rings there, on the speakers chosen in the settings.
* Every call leaves its line in the chat and moves the chat up: which way it went and how long it lasted, or that it was declined or not answered.
* A call's card is dragged to wherever it is wanted in the window, and made larger or smaller by its corner.
* Somebody is added to a call that is on, from its card: a search of the number's chats and contacts. In a call with several and video, each person has a place in the picture. On the native provider.
* The native provider links as "Wuapi", a desktop application, in the phone's list of linked devices (it was an unnamed "other device"). Read by WhatsApp when a number is linked.
* Linking a number: Wuapi Cloud is offered first, with what each place means for the number, and it can be connected from the form. The place a number connects from is chosen from the number and the computer's time zone, and the list to change it is folded.
* Starting with several numbers that have many chats no longer holds the window: a change reads the chats of the number on screen only, and counts the others.
* The provider trait has calls: `start_call`, `accept_call`, `end_call`, `mute_call`, `set_call_camera`, `call_video`, `set_call_devices`, `call_cameras`, `ProviderEvent::Call`, and the `calls` and `video_calls` capabilities.
* Both at once: in a build with `provider-native`, the application starts on the numbers linked to this computer with no sign-in, and Wuapi Cloud is connected from the window (the start screen, or Settings) to add its numbers beside them. Signing out of Wuapi Cloud removes its numbers and leaves the others. A new number is linked to this computer or to the cloud, as chosen in the form. `--provider wuapi` and `--provider native` still run either alone.
* A window with no number yet says what to do first: link a WhatsApp number, with the steps on the phone, and what Wuapi Cloud adds.
* `provider-hub`: a provider made of others, which is what runs the two side by side. The trait has `capabilities_for` and `owner_of` for it.
* A release is built with `provider-native`: started with no `--provider`, the application runs the numbers of this computer and Wuapi Cloud side by side. `--provider wuapi` is the client of wuapi alone, as before, on the database it had; `--no-default-features` builds it without the native provider.
* Building needs Rust 1.94.

## [0.2.0]

* The app is called Wuapi, and its macOS bundle is `Wuapi.app`. The name in the menu bar and the About window is the same.
* A menu bar item on macOS: the chats, the status and the updates, from the menu bar.
* Search does not block: the chat list finds messages a moment after the last key, and a word typed is one question.
* Updates install themselves: an installed copy checks, downloads and asks you to restart. A copy that runs from a disk image is asked once to move to Applications.
* The macOS disk image is designed (`cargo xtask dmg`).
* Communities. A "Communities" filter above the chat list groups the chats of each community under its name, the announcement group first. A group's details say which community it is in. A click on a community opens it: the groups it links (the ones you have not joined are marked), its members, and, for an admin, adding an existing group, making a new group inside it and taking a group out. "New community" in the chat list's menu makes a community.
* Linux: an AppImage in every release, one file that runs without installing. The application tells about new versions instead of replacing it, as with the packages.
* Linux: a one-line installer (`install.sh`), which installs under `~/.local` and keeps the self-updater working.
* Linux: `.deb` and `.rpm` packages, which install under `/usr` and are updated by the system's package manager.

## [0.1.1]

* A number linked from another device appears without restarting.
* Story videos play in the app. If this build has no video runtime, the story says it cannot be played here and does not open another application.
* GIFs and stickers download with pictures, up to 25 MB. A story GIF plays in the viewer once it arrives. A GIF in a chat still opens outside the app when you click it. A normal video still waits for a click. With downloads set to nothing, none of these start on their own.

## [0.1.0]

* First release.
