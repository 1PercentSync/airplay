//! RTSP (plaintext then HAP frames), binary plist, and HAP transient pair-setup.

mod event;
mod pair;
mod plist;
mod rtsp;

pub use event::connect_events;
pub use pair::transient_pair;
pub use plist::{
    decode as plist_decode, encode as plist_encode, pretty_print as pretty_print_value, PlistInt,
    Value,
};
pub use rtsp::{parse_host_port, Identity, Response, RtspClient};
