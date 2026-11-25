#[cfg(all(feature = "embedded-arti", unix))]
mod arti;
mod dns;
mod flow;
mod policy;
mod router;
mod session;
mod tcp;
mod tun;

#[cfg(all(feature = "embedded-arti", unix))]
pub use arti::dial_with_arti;
pub use dns::{DNS_V4, DNS_V6, DnsLeases};
pub use flow::{FLOW_CAPACITY, FLOW_DIRECTION_BYTES, FlowBuffer, STREAM_CHUNK_BYTES};
pub use policy::{Destination, Policy, RouteMode, Scope};
pub use router::{PacketAction, PacketRouter};
pub use session::{AdmissionError, DialTicket, VpnSession};
pub use tcp::TcpStack;
pub use tun::{FlowKey, Packet, PacketError, Transport};

pub const TUN_MTU: usize = 1280;
pub const MAX_PENDING_DIALS: usize = 16;
pub const DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
