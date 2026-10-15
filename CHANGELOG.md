# Changelog

## [Unreleased]

* A GIF plays in its bubble, silent and over and over, as soon as it is downloaded (it was a file tile to open). Under reduced motion, or without the video runtime, the tile stays.
* A video of a conversation plays inside the window: play and hold (Space), the bar to go anywhere in it, five seconds back and forward with the arrows, Save as… and the way to open it with another application.
* Four or more pictures one person sent together are one album: a grid of four in one bubble, with how many more over the last. A click opens a picture in the viewer, where the arrows go through the rest.
* A chat's photos and videos, files and links are listed from its panel ("Media, files and links"): by month, newest first, with who sent each and when, and the way to the message in the conversation.
* A message is pinned at the top of its chat for everyone in it, for 24 hours, 7 days or 30 days ("Pin" in its menu, or P). A bar under the chat's header shows the pinned message and goes to it; a chat keeps three. With the numbers of this computer; Wuapi Cloud's API has no way to pin yet.
* A chat's picture is no longer downloaded again at every start when the chat list names it differently from the picture itself.
* A story's video that could not open the sound card now plays without sound, instead of saying it cannot be played. A second video, story or GIF no longer brings the application down.
* The provider trait has `pin_message`, the `pins` capability, `MessageExtras::pinned_until` and `PinFor`.
* Releases embed the video runtime on every platform: libmpv with FFmpeg, built LGPL-only from pinned sources by `packaging/video/build-libmpv.sh` (`video-runtime.yml`), and required by the release workflow.

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
