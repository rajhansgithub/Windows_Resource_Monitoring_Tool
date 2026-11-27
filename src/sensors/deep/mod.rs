//! Deep per-device facts for the device detail pages (click a Monitor tile). Each module
//! reads one kind of device. Static facts are read once on a worker thread and cached;
//! live values are cheap and read only while that device's page is on screen.

pub mod battery;
pub mod cpu;
pub mod disk;
pub mod display;
pub mod gpu;
pub mod net;
