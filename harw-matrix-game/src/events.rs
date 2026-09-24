//! Beobachter-Events für das Matrix-Panel der TUI (matrix-game.md §7).
//!
//! Die TUI hängt nur über diesen Stream plus Befehlskanal am Kern. Events
//! werden aus Journal-Einträgen abgeleitet und tragen die Audience, damit
//! die TUI Paar-Kanäle mit Schloss und „nur für X & Y sichtbar“ markieren
//! kann. Der Beobachter sieht alles; die Sitz-Filterung passiert nicht hier,
//! sondern in [`crate::visibility::project`].

use serde::{Deserialize, Serialize};

use crate::commitments::{Commitment, Salt, verify};
use crate::dice::{DiceRoll, Grade, Outcome};
use crate::phases::Phase;
use crate::state::{Audience, EntryKind, GameEntry, Journal, PlayerId, Seat, VarValue};

/// Event für die TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MatrixGameEvent {
    /// Neue Runde (Briefing betreten).
    RoundStarted {
        /// Runde.
        round: u32,
    },
    /// Phasenwechsel.
    PhaseEntered {
        /// Runde.
        round: u32,
        /// Phase.
        phase: Phase,
    },
    /// Privater Kanal eröffnet.
    ChannelOpened {
        /// Runde.
        round: u32,
        /// Kanal-ID.
        channel: String,
        /// Mitglieder.
        members: [PlayerId; 2],
    },
    /// Text in einem Protokoll (öffentlich, Kanal, Sitz, Umpire).
    MessagePosted {
        /// Runde.
        round: u32,
        /// Audience.
        audience: Audience,
        /// Absender (None = Rust-Template/Welt).
        from: Option<Seat>,
        /// Kanal bei Paar-Nachrichten.
        channel: Option<String>,
        /// Text.
        text: String,
    },
    /// Versiegelte Einreichung eingegangen.
    ActionSubmitted {
        /// Runde.
        round: u32,
        /// Sitz.
        seat: PlayerId,
        /// Argument-ID.
        argument_id: String,
        /// Commitment.
        commitment: Commitment,
    },
    /// Würfelwurf.
    DiceRolled {
        /// Runde.
        round: u32,
        /// Audience.
        audience: Audience,
        /// Wurf.
        roll: DiceRoll,
    },
    /// Argument entschieden.
    Adjudicated {
        /// Runde.
        round: u32,
        /// Audience.
        audience: Audience,
        /// Argument-ID.
        argument_id: String,
        /// Ergebnis.
        outcome: Outcome,
        /// Grad.
        grade: Option<Grade>,
    },
    /// Weltvariable geändert.
    WorldChanged {
        /// Runde.
        round: u32,
        /// Audience (Sichtbarkeit der Variablen).
        audience: Audience,
        /// Variable.
        var: String,
        /// Alt.
        from: VarValue,
        /// Neu.
        to: VarValue,
    },
    /// Geheimnis offengelegt.
    SecretRevealed {
        /// Runde.
        round: u32,
        /// Geheimnis-ID.
        secret_id: String,
        /// Argument-ID.
        argument_id: String,
        /// Commitment-Prüfung bestanden.
        verified: bool,
    },
    /// Spielende.
    GameEnded {
        /// Runde.
        round: u32,
        /// Grund.
        reason: String,
    },
}

/// Events zu einem Journal-Eintrag (0 bis 2).
#[must_use]
pub fn events_for_entry(entry: &GameEntry) -> Vec<MatrixGameEvent> {
    let round = entry.round;
    let audience = entry.audience.clone();
    let message = |from: Option<Seat>, channel: Option<String>, text: String| {
        MatrixGameEvent::MessagePosted {
            round,
            audience: audience.clone(),
            from,
            channel,
            text,
        }
    };
    match &entry.kind {
        EntryKind::PhaseEntered { phase } => {
            let mut out = Vec::new();
            if *phase == Phase::Briefing {
                out.push(MatrixGameEvent::RoundStarted { round });
            }
            out.push(MatrixGameEvent::PhaseEntered {
                round,
                phase: *phase,
            });
            out
        }
        EntryKind::ChannelOpened {
            channel,
            members,
            initiator,
            opening,
        } => vec![
            MatrixGameEvent::ChannelOpened {
                round,
                channel: channel.clone(),
                members: members.clone(),
            },
            message(
                Some(Seat::Player(initiator.clone())),
                Some(channel.clone()),
                opening.clone(),
            ),
        ],
        EntryKind::NegotiationPosted {
            channel,
            from,
            text,
            ..
        } => vec![message(
            Some(Seat::Player(from.clone())),
            Some(channel.clone()),
            text.clone(),
        )],
        EntryKind::NegotiationClosed => {
            vec![message(None, None, "Verhandlungsphase beendet.".to_owned())]
        }
        EntryKind::ArgumentSealed {
            argument_id,
            seat,
            commitment,
        } => vec![MatrixGameEvent::ActionSubmitted {
            round,
            seat: seat.clone(),
            argument_id: argument_id.clone(),
            commitment: commitment.clone(),
        }],
        EntryKind::ArgumentRevealed { seat, argument, .. } => vec![message(
            Some(Seat::Player(seat.clone())),
            None,
            argument.action.clone(),
        )],
        EntryKind::SecretArgumentAnnounced { text, .. }
        | EntryKind::Forfeit { text, .. }
        | EntryKind::FactAdded { text }
        | EntryKind::InjectApplied { text, .. } => vec![message(None, None, text.clone())],
        EntryKind::Narrated { text, .. } => vec![message(Some(Seat::Umpire), None, text.clone())],
        EntryKind::DiceRolled { roll } => vec![MatrixGameEvent::DiceRolled {
            round,
            audience: entry.audience.clone(),
            roll: roll.clone(),
        }],
        EntryKind::ArgumentResolved {
            argument_id,
            outcome,
            grade,
            ..
        } => vec![MatrixGameEvent::Adjudicated {
            round,
            audience: entry.audience.clone(),
            argument_id: argument_id.clone(),
            outcome: *outcome,
            grade: *grade,
        }],
        EntryKind::WorldDelta { var, from, to, .. } => vec![MatrixGameEvent::WorldChanged {
            round,
            audience: entry.audience.clone(),
            var: var.clone(),
            from: from.clone(),
            to: to.clone(),
        }],
        EntryKind::SecretRevealed {
            secret_id,
            argument_id,
            content,
            salt_hex,
            commitment,
            ..
        } => {
            let verified = Salt::from_hex(salt_hex)
                .and_then(|salt| verify(content, &salt, commitment).ok())
                .unwrap_or(false);
            vec![MatrixGameEvent::SecretRevealed {
                round,
                secret_id: secret_id.clone(),
                argument_id: argument_id.clone(),
                verified,
            }]
        }
        EntryKind::GameEnded { reason } => vec![MatrixGameEvent::GameEnded {
            round,
            reason: reason.clone(),
        }],
        _ => Vec::new(),
    }
}

/// Alle Events eines Journals (Beobachter-Stream, z. B. beim Wiederanhängen).
#[must_use]
pub fn events_for_journal(journal: &Journal) -> Vec<MatrixGameEvent> {
    journal.entries().flat_map(events_for_entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scripted_game;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn scripted_game_emits_all_event_kinds() -> TestResult {
        let (_, log) = scripted_game(&[31u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let events = events_for_journal(&log.journal);
        let has = |pred: &dyn Fn(&MatrixGameEvent) -> bool| events.iter().any(pred);
        assert!(has(&|e| matches!(
            e,
            MatrixGameEvent::RoundStarted { round: 1 }
        )));
        assert!(has(&|e| matches!(e, MatrixGameEvent::PhaseEntered { .. })));
        assert!(has(&|e| matches!(e, MatrixGameEvent::ChannelOpened { .. })));
        assert!(has(&|e| matches!(
            e,
            MatrixGameEvent::MessagePosted {
                audience: Audience::Pair(..),
                ..
            }
        )));
        assert!(has(&|e| matches!(
            e,
            MatrixGameEvent::ActionSubmitted { .. }
        )));
        assert!(has(&|e| matches!(e, MatrixGameEvent::DiceRolled { .. })));
        assert!(has(&|e| matches!(e, MatrixGameEvent::Adjudicated { .. })));
        assert!(has(&|e| matches!(e, MatrixGameEvent::WorldChanged { .. })));
        assert!(has(&|e| matches!(
            e,
            MatrixGameEvent::SecretRevealed { verified: true, .. }
        )));
        assert!(has(&|e| matches!(e, MatrixGameEvent::GameEnded { .. })));
        // JSON-Roundtrip für die TUI-Anbindung
        for e in &events {
            let json = serde_json::to_string(e)?;
            let back: MatrixGameEvent = serde_json::from_str(&json)?;
            assert_eq!(&back, e);
        }
        Ok(())
    }
}
