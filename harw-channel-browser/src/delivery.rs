/// The durable delivery status of one outbound channel intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// The intent exists, but the browser has not submitted it yet.
    Pending,
    /// Submission succeeded locally and external confirmation is still absent.
    AwaitingConfirmation,
    /// The external system confirmed the message and supplied its identity.
    Delivered,
    /// Confirmation could not be established before the observation deadline.
    Uncertain,
}

/// Observable facts produced while delivering an outbound channel message.
#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryEvidence {
    /// The browser accepted the submit action.
    ///
    /// This is deliberately not evidence that the external system delivered
    /// the message.
    SubmitAccepted,
    /// A matching network response confirmed the external message.
    NetworkConfirmed { external_message_id: String },
    /// A matching DOM observation confirmed the external message.
    DomConfirmed { external_message_id: String },
    /// No authoritative confirmation arrived within the bounded deadline.
    ConfirmationTimedOut,
}

/// State owned by the connector while an outbound message is being delivered.
#[derive(Debug, PartialEq, Eq)]
pub struct PendingDelivery {
    conversation_id: String,
    client_intent_id: String,
    state: DeliveryState,
    external_message_id: Option<String>,
}

impl PendingDelivery {
    #[must_use]
    pub fn new(conversation_id: impl Into<String>, client_intent_id: impl Into<String>) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            client_intent_id: client_intent_id.into(),
            state: DeliveryState::Pending,
            external_message_id: None,
        }
    }

    #[must_use]
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    #[must_use]
    pub fn client_intent_id(&self) -> &str {
        &self.client_intent_id
    }

    #[must_use]
    pub fn state(&self) -> DeliveryState {
        self.state
    }

    #[must_use]
    pub fn external_message_id(&self) -> Option<&str> {
        self.external_message_id.as_deref()
    }

    /// Consume the current value and apply one observed delivery fact.
    ///
    /// Confirmed delivery is terminal: later timeout or submit observations
    /// cannot weaken authoritative network or DOM evidence.
    #[must_use]
    pub fn observe(mut self, evidence: DeliveryEvidence) -> Self {
        if self.state == DeliveryState::Delivered {
            return self;
        }

        match evidence {
            DeliveryEvidence::SubmitAccepted => {
                self.state = DeliveryState::AwaitingConfirmation;
            }
            DeliveryEvidence::NetworkConfirmed {
                external_message_id,
            }
            | DeliveryEvidence::DomConfirmed {
                external_message_id,
            } => {
                self.state = DeliveryState::Delivered;
                self.external_message_id = Some(external_message_id);
            }
            DeliveryEvidence::ConfirmationTimedOut => {
                self.state = DeliveryState::Uncertain;
            }
        }

        self
    }
}
