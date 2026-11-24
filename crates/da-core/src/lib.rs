//! DisplayAnything core: device identity, trust store, pairing and authenticated
//! LAN connections. Later milestones add the video and input channels on top
//! of the session established here.

pub mod autoconnect;
pub mod discovery;
pub mod identity;
pub mod net;
pub mod pairing;
pub mod protocol;
pub mod tls;
pub mod trust;

pub use autoconnect::AutoConnector;
pub use identity::{Fingerprint, Identity};
pub use net::{Config, Event, Node, Session};
pub use trust::{Permissions, TrustStore, TrustedPeer};

/// Default TCP port for the control channel.
pub const DEFAULT_PORT: u16 = 47800;
/// Protocol version spoken on the control channel.
pub const PROTOCOL_VERSION: u32 = 1;
