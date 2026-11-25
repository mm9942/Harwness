use std::collections::HashMap;
use std::time::Instant;

use super::{DIAL_TIMEOUT, Destination, FLOW_CAPACITY, FlowKey, MAX_PENDING_DIALS, Policy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Denied,
    AlreadyTracked,
    FlowLimit,
    DialLimit,
    SessionExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialTicket {
    pub flow: FlowKey,
    pub deadline: Instant,
    serial: u64,
}

struct Flow {
    ticket: DialTicket,
    connected: bool,
}

/// Admission and dial lifecycle for the single TUN owner.
pub struct VpnSession {
    policy: Policy,
    flows: HashMap<FlowKey, Flow>,
    serial: u64,
    pending: usize,
}

impl VpnSession {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            flows: HashMap::with_capacity(FLOW_CAPACITY),
            serial: 0,
            pending: 0,
        }
    }

    pub fn contains(&self, flow: FlowKey) -> bool {
        self.flows.contains_key(&flow)
    }

    pub fn admit(
        &mut self,
        flow: FlowKey,
        uid: Option<u32>,
        destination: &Destination,
        now: Instant,
    ) -> Result<DialTicket, AdmissionError> {
        if !self.policy.permits(uid, destination) {
            return Err(AdmissionError::Denied);
        }
        if self.flows.contains_key(&flow) {
            return Err(AdmissionError::AlreadyTracked);
        }
        if self.flows.len() >= FLOW_CAPACITY {
            return Err(AdmissionError::FlowLimit);
        }
        if self.pending >= MAX_PENDING_DIALS {
            return Err(AdmissionError::DialLimit);
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or(AdmissionError::SessionExhausted)?;
        let ticket = DialTicket {
            flow,
            deadline: now + DIAL_TIMEOUT,
            serial: self.serial,
        };
        self.flows.insert(
            flow,
            Flow {
                ticket,
                connected: false,
            },
        );
        self.pending += 1;
        Ok(ticket)
    }

    /// Accept a completed dial only if its ticket is current and within deadline.
    pub fn connected(&mut self, ticket: DialTicket, now: Instant) -> bool {
        let Some(flow) = self.flows.get_mut(&ticket.flow) else {
            return false;
        };
        if flow.ticket != ticket || flow.connected || now >= flow.ticket.deadline {
            return false;
        }
        flow.connected = true;
        self.pending -= 1;
        true
    }

    pub fn close(&mut self, ticket: DialTicket) -> bool {
        if !self
            .flows
            .get(&ticket.flow)
            .is_some_and(|flow| flow.ticket == ticket)
        {
            return false;
        }
        if let Some(flow) = self.flows.remove(&ticket.flow) {
            self.pending -= usize::from(!flow.connected);
        }
        true
    }

    pub fn expire_dials(&mut self, now: Instant) {
        self.flows.retain(|_, flow| {
            let expired = !flow.connected && now >= flow.ticket.deadline;
            self.pending -= usize::from(expired);
            !expired
        });
    }

    /// Invalidate all existing tickets before accepting connections under new rules.
    pub fn replace_policy(&mut self, policy: Policy) {
        self.policy = policy;
        self.flows.clear();
        self.pending = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::{RouteMode, Scope};

    fn policy() -> Policy {
        Policy::new(
            Scope::DeviceWide(RouteMode::InternetAndOnion),
            Default::default(),
        )
    }

    fn flow(port: u16) -> FlowKey {
        FlowKey {
            source: ([10, 8, 0, 2], port).into(),
            destination: ([1, 1, 1, 1], 443).into(),
        }
    }

    #[test]
    fn dial_cap_and_timeout_reclaim_slots() {
        let now = Instant::now();
        let mut session = VpnSession::new(policy());
        for port in 1..=MAX_PENDING_DIALS as u16 {
            session
                .admit(
                    flow(port),
                    Some(1),
                    &Destination::PublicIp(([1, 1, 1, 1], 443).into()),
                    now,
                )
                .unwrap();
        }
        let next = flow(2000);
        assert_eq!(
            session.admit(
                next,
                Some(1),
                &Destination::PublicIp(([1, 1, 1, 1], 443).into()),
                now
            ),
            Err(AdmissionError::DialLimit)
        );
        session.expire_dials(now + DIAL_TIMEOUT);
        assert!(
            session
                .admit(
                    next,
                    Some(1),
                    &Destination::PublicIp(([1, 1, 1, 1], 443).into()),
                    now
                )
                .is_ok()
        );
    }

    #[test]
    fn policy_replacement_rejects_stale_completion_for_reused_tuple() {
        let now = Instant::now();
        let mut session = VpnSession::new(policy());
        let flow = flow(2000);
        let old = session
            .admit(
                flow,
                Some(1),
                &Destination::PublicIp(([1, 1, 1, 1], 443).into()),
                now,
            )
            .unwrap();
        session.replace_policy(policy());
        let new = session
            .admit(
                flow,
                Some(1),
                &Destination::PublicIp(([1, 1, 1, 1], 443).into()),
                now,
            )
            .unwrap();
        assert!(!session.connected(old, now));
        assert!(!session.close(old));
        assert!(session.connected(new, now));
        assert!(session.close(new));
    }
}
