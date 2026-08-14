//! WASAPI render-endpoint enumeration, loopback capture, resample, TPDF pack.

mod capture;
mod enum_devices;
#[cfg(windows)]
mod policy_format;
mod policy;
mod process;
mod ring;
mod sessions;

pub use capture::Capture;
pub use enum_devices::{
    default_render_device_id, list_render_devices, pick_render_device_id, RenderDevice,
};
#[cfg(windows)]
pub use policy_format::FormatGuard;
pub use policy::set_default_render_device;
pub use process::spawn_processor;
pub use ring::{PacketQueue, SampleRing};
pub use sessions::browser_active_on;
