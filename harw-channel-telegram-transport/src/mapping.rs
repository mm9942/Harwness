//! Pure Telegram Bot API wire mapping and closed command grammar.

use harw_channel::{AttachmentRef, ChannelId, InboundEvent, PeerId, SenderRef, ThreadRef};
use jiff::Timestamp;
use serde::Deserialize;

/// The transport kind used until ingress supplies a configured binding id.
const TELEGRAM_CHANNEL_ID: &str = "telegram";

/// The Telegram update fields relevant to inbound mapping.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<RawMessage>,
    #[serde(default)]
    pub edited_message: Option<RawMessage>,
    #[serde(default)]
    pub callback_query: Option<RawCallbackQuery>,
}

/// A Telegram message, deliberately limited to data accepted at the transport boundary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawMessage {
    pub message_id: i64,
    pub chat: RawChat,
    #[serde(default)]
    pub from: Option<RawUser>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub entities: Vec<RawMessageEntity>,
    #[serde(default)]
    pub caption_entities: Vec<RawMessageEntity>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub reply_to_message: Option<Box<RawMessage>>,
    #[serde(default)]
    pub document: Option<RawDocument>,
    #[serde(default)]
    pub photo: Vec<RawPhotoSize>,
    #[serde(default)]
    pub audio: Option<RawMedia>,
    #[serde(default)]
    pub video: Option<RawMedia>,
    #[serde(default)]
    pub voice: Option<RawMedia>,
    #[serde(default)]
    pub animation: Option<RawMedia>,
}

/// The chat identity attached to a Telegram message.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// A Telegram user identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawUser {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

/// A button tap. Its `data` remains opaque: it is not mapped into user text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawCallbackQuery {
    pub id: String,
    pub from: RawUser,
    #[serde(default)]
    pub message: Option<RawMessage>,
    #[serde(default)]
    pub data: Option<String>,
}

/// Telegram's UTF-16 indexed message entity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawMessageEntity {
    #[serde(rename = "type")]
    pub kind: String,
    pub offset: u32,
    pub length: u32,
    #[serde(default)]
    pub user: Option<RawUser>,
}

/// A Telegram document attachment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawDocument {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// A Telegram photo representation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawPhotoSize {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// Shared wire fields for audio, video, voice, and animation attachments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawMedia {
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_name: Option<String>,
}

/// A parsed command whose arguments are still untrusted transport input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramCommand {
    Request {
        workspace_alias: String,
        role: String,
        task: String,
    },
    Review {
        work_id: String,
    },
    Approve {
        work_id: String,
    },
    Deny {
        work_id: String,
    },
    Cancel {
        work_id: String,
    },
}

/// Maps a supported Telegram update into the transport-neutral ingress event.
///
/// Callback payload data is intentionally excluded: it must be validated by the
/// pending-approval boundary, never interpreted as ordinary message content.
/// Callback-Queries werden separat über [`map_callback_query`] abgebildet.
#[must_use]
pub fn map_update(
    update: &RawUpdate,
    bot_id: i64,
    bot_username: Option<&str>,
) -> Option<InboundEvent> {
    if let Some(message) = update.message.as_ref().or(update.edited_message.as_ref()) {
        return map_message(
            update.update_id,
            message,
            message.from.as_ref(),
            bot_id,
            bot_username,
        );
    }

    // Callback payloads are a separate approval-response protocol. Mapping the
    // bot's original message as fresh human input would be unsafe.
    None
}

/// Parses only the closed remote-command syntax; it performs no resolution.
///
/// Die Gruppenform `/cmd@botname args` wird akzeptiert: ein `@username`-Suffix
/// am Befehlstoken wird entfernt, sofern er nicht leer ist und nur aus dem
/// Telegram-Username-Zeichensatz `[A-Za-z0-9_]` besteht. Welcher Bot adressiert
/// ist, wird hier bewusst nicht geprüft; die Adressierung entscheidet die
/// Mention-/Admission-Logik.
#[must_use]
pub fn parse_command(text: &str) -> Option<TelegramCommand> {
    let text = text.trim();
    let (command, arguments) = text.split_once(char::is_whitespace)?;
    let command = strip_bot_suffix(command)?;

    match command {
        "/request" => parse_request(arguments),
        "/review" => parse_work_id(arguments).map(|work_id| TelegramCommand::Review { work_id }),
        "/approve" => parse_work_id(arguments).map(|work_id| TelegramCommand::Approve { work_id }),
        "/deny" => parse_work_id(arguments).map(|work_id| TelegramCommand::Deny { work_id }),
        "/cancel" => parse_work_id(arguments).map(|work_id| TelegramCommand::Cancel { work_id }),
        _ => None,
    }
}

/// Entfernt ein optionales `@username`-Suffix vom Befehlstoken.
///
/// Liefert `None`, wenn nach `@` kein gültiger Telegram-Username folgt.
fn strip_bot_suffix(command: &str) -> Option<&str> {
    match command.split_once('@') {
        None => Some(command),
        Some((command, username)) => {
            let valid = !username.is_empty()
                && username
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
            valid.then_some(command)
        }
    }
}

/// Ein Tastendruck auf einen Inline-Button, getrennt vom normalen Nachrichtenpfad.
///
/// `data` bleibt opak und unvertrauenswürdig: Sie darf nur von der
/// Pending-Approval-Grenze (Token-Store) ausgewertet werden, nie als Nutzertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramCallback {
    /// Telegram `update_id` für Dedup/Offset.
    pub update_id: i64,
    /// `callback_query.id`, benötigt für `answerCallbackQuery`.
    pub callback_id: String,
    /// Opake Callback-Nutzlast des Buttons.
    pub data: String,
    /// Chat der Nachricht, an der der Button hing.
    pub chat_id: i64,
    /// Nachricht, an der der Button hing.
    pub message_id: i64,
    /// Forum-Thema der Nachricht, falls vorhanden.
    pub thread_id: Option<i64>,
    /// Der tatsächlich tippende Nutzer (nicht der Autor der Nachricht).
    pub sender: SenderRef,
}

/// Bildet eine `callback_query` auf einen [`TelegramCallback`] ab.
///
/// Liefert `None`, wenn das Update keine Callback-Query enthält, keine
/// (zugängliche) Nachricht trägt – ohne Chat-/Nachrichtenbindung lässt sich
/// ein Approval-Token nicht sicher zuordnen – oder keine nicht-leere `data`
/// besitzt.
#[must_use]
pub fn map_callback_query(update: &RawUpdate) -> Option<TelegramCallback> {
    let callback = update.callback_query.as_ref()?;
    let message = callback.message.as_ref()?;
    let data = callback.data.as_deref().filter(|data| !data.is_empty())?;
    if callback.id.is_empty() {
        return None;
    }
    Some(TelegramCallback {
        update_id: update.update_id,
        callback_id: callback.id.clone(),
        data: data.to_owned(),
        chat_id: message.chat.id,
        message_id: message.message_id,
        thread_id: message.message_thread_id,
        sender: sender_ref(&callback.from),
    })
}

fn map_message(
    update_id: i64,
    message: &RawMessage,
    sender: Option<&RawUser>,
    bot_id: i64,
    bot_username: Option<&str>,
) -> Option<InboundEvent> {
    let text = message.text.as_ref().or(message.caption.as_ref());
    let mentioned =
        text.is_some_and(|text| {
            has_bot_mention(text, entities_for_message(message), bot_id, bot_username)
        }) || reply_is_from_bot(message.reply_to_message.as_deref(), bot_id, bot_username);

    Some(InboundEvent {
        channel: ChannelId::from_str(TELEGRAM_CHANNEL_ID),
        peer: PeerId::from_str(message.chat.id.to_string()),
        thread: message
            .message_thread_id
            .map(|thread_id| ThreadRef::from_str(thread_id.to_string())),
        sender: sender.map(sender_ref),
        text: text.cloned(),
        mentioned,
        attachments: attachments(message),
        raw_event_id: Some(update_id.to_string()),
        received_at: Timestamp::now(),
    })
}

fn entities_for_message(message: &RawMessage) -> &[RawMessageEntity] {
    if message.text.is_some() {
        &message.entities
    } else {
        &message.caption_entities
    }
}

fn sender_ref(user: &RawUser) -> SenderRef {
    SenderRef {
        id: user.id.to_string(),
        display_name: display_name(user),
    }
}

fn display_name(user: &RawUser) -> Option<String> {
    let mut name = user.first_name.trim().to_owned();
    if let Some(last_name) = user
        .last_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(last_name);
    }
    (!name.is_empty())
        .then_some(name)
        .or_else(|| user.username.clone())
}

fn has_bot_mention(
    text: &str,
    entities: &[RawMessageEntity],
    bot_id: i64,
    bot_username: Option<&str>,
) -> bool {
    entities.iter().any(|entity| {
        if entity.kind.eq_ignore_ascii_case("text_mention") {
            return entity.user.as_ref().is_some_and(|user| user.id == bot_id);
        }

        if !entity.kind.eq_ignore_ascii_case("mention") {
            return false;
        }

        let Some(bot_username) = normalized_username(bot_username) else {
            return false;
        };
        let Some(mention) = utf16_slice(text, entity.offset, entity.length) else {
            return false;
        };

        mention
            .strip_prefix('@')
            .is_some_and(|username| username.eq_ignore_ascii_case(bot_username))
    })
}

fn reply_is_from_bot(reply: Option<&RawMessage>, bot_id: i64, bot_username: Option<&str>) -> bool {
    let Some(author) = reply.and_then(|message| message.from.as_ref()) else {
        return false;
    };
    author.id == bot_id
        || normalized_username(bot_username).is_some_and(|bot_username| {
            author
                .username
                .as_deref()
                .is_some_and(|username| username.eq_ignore_ascii_case(bot_username))
        })
}

fn normalized_username(username: Option<&str>) -> Option<&str> {
    let username = username?.trim();
    let username = username.strip_prefix('@').unwrap_or(username);
    (!username.is_empty()).then_some(username)
}

fn utf16_slice(text: &str, offset: u32, length: u32) -> Option<&str> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(length).ok()?)?;
    let byte_start = utf16_offset_to_byte_index(text, start)?;
    let byte_end = utf16_offset_to_byte_index(text, end)?;
    (byte_start <= byte_end).then(|| &text[byte_start..byte_end])
}

/// Resolves a Telegram UTF-16 offset only when it lands on a Unicode scalar boundary.
///
/// An offset in the middle of a surrogate pair is malformed input, so it deliberately
/// does not resolve to a byte index.
fn utf16_offset_to_byte_index(text: &str, offset: usize) -> Option<usize> {
    let mut utf16_position = 0usize;

    for (byte_index, character) in text.char_indices() {
        if utf16_position == offset {
            return Some(byte_index);
        }
        utf16_position = utf16_position.checked_add(character.len_utf16())?;
    }

    (utf16_position == offset).then_some(text.len())
}

fn attachments(message: &RawMessage) -> Vec<AttachmentRef> {
    let mut attachments = Vec::new();
    if let Some(document) = message.document.as_ref() {
        push_attachment(
            &mut attachments,
            &document.file_id,
            document.mime_type.clone(),
            document.file_size,
            document.file_name.clone(),
        );
    }
    if let Some(photo) = message.photo.last() {
        push_attachment(
            &mut attachments,
            &photo.file_id,
            None,
            photo.file_size,
            None,
        );
    }
    for media in [
        message.audio.as_ref(),
        message.video.as_ref(),
        message.voice.as_ref(),
        message.animation.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        push_attachment(
            &mut attachments,
            &media.file_id,
            media.mime_type.clone(),
            media.file_size,
            media.file_name.clone(),
        );
    }
    attachments
}

fn push_attachment(
    attachments: &mut Vec<AttachmentRef>,
    remote_id: &str,
    reported_mime: Option<String>,
    declared_size: Option<u64>,
    filename: Option<String>,
) {
    if remote_id.trim().is_empty() {
        return;
    }
    attachments.push(AttachmentRef {
        remote_id: remote_id.to_owned(),
        reported_mime,
        declared_size,
        filename,
    });
}

fn parse_request(arguments: &str) -> Option<TelegramCommand> {
    let arguments = arguments.trim();
    let (workspace_alias, rest) = arguments.split_once(char::is_whitespace)?;
    let (role, task) = rest.trim_start().split_once(char::is_whitespace)?;
    let task = task.trim();
    if !is_workspace_alias(workspace_alias) || !is_atom(role) || task.is_empty() {
        return None;
    }
    Some(TelegramCommand::Request {
        workspace_alias: workspace_alias.to_owned(),
        role: role.to_owned(),
        task: task.to_owned(),
    })
}

fn parse_work_id(arguments: &str) -> Option<String> {
    let work_id = arguments.trim();
    is_atom(work_id).then(|| work_id.to_owned())
}

fn is_workspace_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.is_ascii()
        && !alias.contains('/')
        && !alias.contains("..")
        && !alias.contains('~')
        && !alias.starts_with('-')
        && !alias.ends_with('-')
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_atom(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use serde_json::json;

    fn message() -> TestResult<RawMessage> {
        serde_json::from_value(json!({
            "message_id": 7,
            "chat": { "id": -10042, "type": "supergroup", "title": "Ops" },
            "from": { "id": 99, "is_bot": false, "first_name": "Mia", "username": "mia" },
            "text": "hello @HarwBot",
            "entities": [{ "type": "mention", "offset": 6, "length": 8 }],
            "message_thread_id": 33,
            "document": { "file_id": "doc-1", "file_name": "brief.txt", "mime_type": "text/plain", "file_size": 12 },
            "photo": [{ "file_id": "photo-small", "file_size": 1 }, { "file_id": "photo-large", "file_size": 2 }]
        }))
        .map_err(ctx("test message JSON is valid"))
    }

    #[test]
    fn serde_mapping_preserves_routing_identity_and_conservative_attachments() -> TestResult {
        let update = RawUpdate {
            update_id: 123,
            message: Some(message()?),
            edited_message: None,
            callback_query: None,
        };

        let event =
            map_update(&update, 700, Some("@harwbot")).ok_or(TestError::Missing("message maps"))?;
        assert_eq!(event.channel.as_str(), "telegram");
        assert_eq!(event.peer.as_str(), "-10042");
        assert_eq!(event.thread.as_ref().map(ThreadRef::as_str), Some("33"));
        assert_eq!(
            event.sender.as_ref().map(|sender| sender.id.as_str()),
            Some("99")
        );
        assert_eq!(
            event.sender.and_then(|sender| sender.display_name),
            Some("Mia".to_owned())
        );
        assert_eq!(event.text.as_deref(), Some("hello @HarwBot"));
        assert!(event.mentioned);
        assert_eq!(event.raw_event_id.as_deref(), Some("123"));
        assert_eq!(event.attachments.len(), 2);
        assert_eq!(event.attachments[0].remote_id, "doc-1");
        assert_eq!(event.attachments[1].remote_id, "photo-large");
        Ok(())
    }

    #[test]
    fn mapping_uses_caption_entities_and_reply_to_bot_but_not_unstructured_text() -> TestResult {
        let mut caption_message = message()?;
        caption_message.text = None;
        caption_message.entities.clear();
        caption_message.caption = Some("look @harwbot".to_owned());
        caption_message.caption_entities = vec![RawMessageEntity {
            kind: "Mention".to_owned(),
            offset: 5,
            length: 8,
            user: None,
        }];
        let caption_event = map_update(
            &RawUpdate {
                update_id: 1,
                message: Some(caption_message),
                edited_message: None,
                callback_query: None,
            },
            700,
            Some("@harwbot"),
        )
        .ok_or(TestError::Missing("caption maps"))?;
        assert!(caption_event.mentioned);

        let mut reply_message = message()?;
        reply_message.text = Some("not an @harwbot mention without an entity".to_owned());
        reply_message.entities.clear();
        reply_message.reply_to_message = Some(Box::new(RawMessage {
            message_id: 6,
            chat: reply_message.chat.clone(),
            from: Some(RawUser {
                id: 700,
                is_bot: true,
                first_name: "Harw".to_owned(),
                last_name: None,
                username: Some("harwbot".to_owned()),
            }),
            text: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            message_thread_id: None,
            reply_to_message: None,
            document: None,
            photo: Vec::new(),
            audio: None,
            video: None,
            voice: None,
            animation: None,
        }));
        let reply_event = map_update(
            &RawUpdate {
                update_id: 2,
                message: Some(reply_message),
                edited_message: None,
                callback_query: None,
            },
            700,
            Some("harwbot"),
        )
        .ok_or(TestError::Missing("reply maps"))?;
        assert!(reply_event.mentioned);
        Ok(())
    }

    #[test]
    fn callback_is_serde_deserializable_but_not_mapped_as_message_input() -> TestResult {
        let update = RawUpdate {
            update_id: 22,
            message: None,
            edited_message: None,
            callback_query: Some(RawCallbackQuery {
                id: "callback-1".to_owned(),
                from: RawUser {
                    id: 44,
                    is_bot: false,
                    first_name: "Operator".to_owned(),
                    last_name: None,
                    username: None,
                },
                message: Some(message()?),
                data: Some("opaque-approval-token".to_owned()),
            }),
        };
        assert!(map_update(&update, 700, Some("harwbot")).is_none());
        Ok(())
    }

    #[test]
    fn unsupported_update_without_message_is_dropped() {
        let update = RawUpdate {
            update_id: 1,
            message: None,
            edited_message: None,
            callback_query: None,
        };
        assert!(map_update(&update, 700, Some("harwbot")).is_none());
    }

    #[test]
    fn parses_each_command_variant() {
        assert_eq!(
            parse_command("/request ops-room implementer fix the failing test"),
            Some(TelegramCommand::Request {
                workspace_alias: "ops-room".to_owned(),
                role: "implementer".to_owned(),
                task: "fix the failing test".to_owned()
            })
        );
        assert_eq!(
            parse_command("/review work-42"),
            Some(TelegramCommand::Review {
                work_id: "work-42".to_owned()
            })
        );
        assert_eq!(
            parse_command("/approve work-42"),
            Some(TelegramCommand::Approve {
                work_id: "work-42".to_owned()
            })
        );
        assert_eq!(
            parse_command("/deny work-42"),
            Some(TelegramCommand::Deny {
                work_id: "work-42".to_owned()
            })
        );
        assert_eq!(
            parse_command("/cancel work-42"),
            Some(TelegramCommand::Cancel {
                work_id: "work-42".to_owned()
            })
        );
    }

    #[test]
    fn rejects_malformed_commands_and_unsafe_workspace_aliases() {
        for input in [
            "/request",
            "/request ops role",
            "/request ops  task",
            "/request /srv role task",
            "/request ../ops role task",
            "/request ops/other role task",
            "/request ~ role task",
            "/request https://example.com role task",
            "/request C: role task",
            "/request ops_room role task",
            "/request ops..room role task",
            "/request über role task",
            "/request -ops role task",
            "/request ops- role task",
            "/review",
            "/review work extra",
            "/approve work extra",
            "/deny work extra",
            "/cancel work extra",
            "/request@ ops role task",
            "/request@harw-bot ops role task",
            "/request@harw.bot ops role task",
            "/request@harw@bot ops role task",
            "/unknown@harwbot work-42",
            "/review@harwbot",
            "hello",
        ] {
            assert_eq!(parse_command(input), None, "{input}");
        }
    }

    #[test]
    fn accepts_group_form_with_bot_username_suffix() {
        assert_eq!(
            parse_command("/request@HarwBot_2 ops-room implementer fix it"),
            Some(TelegramCommand::Request {
                workspace_alias: "ops-room".to_owned(),
                role: "implementer".to_owned(),
                task: "fix it".to_owned()
            })
        );
        assert_eq!(
            parse_command("  /approve@harwbot work-42  "),
            Some(TelegramCommand::Approve {
                work_id: "work-42".to_owned()
            })
        );
        assert_eq!(
            parse_command("/cancel@harwbot\twork-7"),
            Some(TelegramCommand::Cancel {
                work_id: "work-7".to_owned()
            })
        );
    }

    fn callback_update(data: Option<&str>, with_message: bool) -> TestResult<RawUpdate> {
        Ok(RawUpdate {
            update_id: 22,
            message: None,
            edited_message: None,
            callback_query: Some(RawCallbackQuery {
                id: "callback-1".to_owned(),
                from: RawUser {
                    id: 44,
                    is_bot: false,
                    first_name: "Operator".to_owned(),
                    last_name: Some("One".to_owned()),
                    username: None,
                },
                message: if with_message { Some(message()?) } else { None },
                data: data.map(str::to_owned),
            }),
        })
    }

    #[test]
    fn callback_query_maps_binding_context_and_tapping_sender() -> TestResult {
        let update = callback_update(Some("opaque-approval-token"), true)?;
        let callback = map_callback_query(&update).ok_or(TestError::Missing("callback maps"))?;
        assert_eq!(
            callback,
            TelegramCallback {
                update_id: 22,
                callback_id: "callback-1".to_owned(),
                data: "opaque-approval-token".to_owned(),
                chat_id: -10042,
                message_id: 7,
                thread_id: Some(33),
                sender: SenderRef {
                    id: "44".to_owned(),
                    display_name: Some("Operator One".to_owned()),
                },
            }
        );
        Ok(())
    }

    #[test]
    fn callback_query_without_message_or_data_is_dropped() -> TestResult {
        assert!(map_callback_query(&callback_update(Some("token"), false)?).is_none());
        assert!(map_callback_query(&callback_update(None, true)?).is_none());
        assert!(map_callback_query(&callback_update(Some(""), true)?).is_none());
        let plain_message = RawUpdate {
            update_id: 1,
            message: Some(message()?),
            edited_message: None,
            callback_query: None,
        };
        assert!(map_callback_query(&plain_message).is_none());
        Ok(())
    }

    #[test]
    fn callback_query_deserializes_from_wire_json() -> TestResult {
        let update: RawUpdate = serde_json::from_value(json!({
            "update_id": 5,
            "callback_query": {
                "id": "cb-9",
                "from": { "id": 44, "is_bot": false, "first_name": "Op" },
                "message": {
                    "message_id": 12,
                    "chat": { "id": 99, "type": "private" }
                },
                "data": "tok"
            }
        }))
        .map_err(ctx("callback update JSON is valid"))?;
        let callback = map_callback_query(&update).ok_or(TestError::Missing("callback maps"))?;
        assert_eq!(callback.chat_id, 99);
        assert_eq!(callback.message_id, 12);
        assert_eq!(callback.thread_id, None);
        assert_eq!(callback.data, "tok");
        assert!(map_update(&update, 700, Some("harwbot")).is_none());
        Ok(())
    }

    #[test]
    fn utf16_entity_offsets_support_non_ascii_prefixes() -> TestResult {
        let mut message = message()?;
        message.text = Some("🙂 @harwbot".to_owned());
        message.entities = vec![RawMessageEntity {
            kind: "mention".to_owned(),
            offset: 3,
            length: 8,
            user: None,
        }];
        let event = map_update(
            &RawUpdate {
                update_id: 1,
                message: Some(message),
                edited_message: None,
                callback_query: None,
            },
            700,
            Some("harwbot"),
        )
        .ok_or(TestError::Missing("message maps"))?;
        assert!(event.mentioned);
        Ok(())
    }

    #[test]
    fn mention_entity_compares_the_extracted_username_not_unstructured_text() {
        assert!(has_bot_mention(
            "🙂 @harwbot",
            &[RawMessageEntity {
                kind: "mention".to_owned(),
                offset: 3,
                length: 8,
                user: None,
            }],
            700,
            Some("@HarwBot"),
        ));
        assert!(!has_bot_mention(
            "🙂 @someoneelse",
            &[RawMessageEntity {
                kind: "mention".to_owned(),
                offset: 3,
                length: 12,
                user: None,
            }],
            700,
            Some("harwbot"),
        ));
    }

    #[test]
    fn utf16_slices_require_scalar_boundaries_and_include_the_text_end() {
        assert_eq!(utf16_slice("🙂 @harwbot", 3, 8), Some("@harwbot"));
        assert_eq!(utf16_slice("🙂 @harwbot", 1, 8), None);
        assert_eq!(utf16_slice("🙂 @harwbot", 11, 0), Some(""));
    }

    #[test]
    fn text_mention_entity_uses_bot_id_without_a_username() -> TestResult {
        let mut message = message()?;
        message.text = Some("for Harw".to_owned());
        message.entities = vec![RawMessageEntity {
            kind: "text_mention".to_owned(),
            offset: 4,
            length: 4,
            user: Some(RawUser {
                id: 700,
                is_bot: true,
                first_name: "Harw".to_owned(),
                last_name: None,
                username: None,
            }),
        }];
        let event = map_update(
            &RawUpdate {
                update_id: 1,
                message: Some(message),
                edited_message: None,
                callback_query: None,
            },
            700,
            None,
        )
        .ok_or(TestError::Missing("message maps"))?;
        assert!(event.mentioned);
        Ok(())
    }
}
