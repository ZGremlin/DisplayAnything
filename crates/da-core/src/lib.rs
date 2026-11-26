//! DisplayAnything core: device identity, trust store, pairing and authenticated
//! LAN connections. Later milestones add the video and input channels on top
//! of the session established here.

pub mod autoconnect;
pub mod discovery;
pub mod identity;
pub mod input;
pub mod net;
pub mod pairing;
pub mod paths;
pub mod protocol;
pub mod remote;
pub mod share;
pub mod tls;
pub mod trust;
pub mod wol;

pub use autoconnect::AutoConnector;
pub use da_input;
pub use da_screen;
pub use identity::{Fingerprint, Identity};
pub use input::{InputLink, InputTarget, PlatformInputTarget};
pub use net::{Config, Event, Node, Session};
pub use paths::{default_data_dir, device_name, parse_addr};
pub use remote::{RemoteControl, RemoteView, ScreenSource, ScreenStarter};
pub use share::{InputShare, ShareStatus};
pub use trust::{Permissions, TrustStore, TrustedPeer};
pub use wol::{MacAddr, WakeReport};

/// Default TCP port for the control channel.
pub const DEFAULT_PORT: u16 = 47800;
/// Protocol version spoken on the control channel.
pub const PROTOCOL_VERSION: u32 = 1;
