//! Process-private CoreAudio endpoint for native lifecycle acceptance.
#![cfg(target_os = "macos")]

// Rust guideline compliant 2026-10-07
use cpal::traits::{DeviceTrait, HostTrait};
use objc2_core_audio::{AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFString, CFType};
use std::ptr::NonNull;
/// An owned aggregate, removed when dropped.
pub struct Aggregate(Option<u32>);
impl Aggregate {
    /// Creates an aggregate backed by the given native device UID.
    ///
    /// # Panics
    /// Panics if CoreAudio refuses creation.
    pub fn create(uid: &str, member: &str) -> Self {
        let member = CFString::from_str(member);
        let member_key = CFString::from_str("uid");
        let child = CFDictionary::from_slices(&[&*member_key], &[&*member]);
        let members = CFArray::from_objects(&[&*child]);
        let uid = CFString::from_str(uid);
        let name = CFString::from_str("Vivid private endpoint check");
        let private = CFNumber::new_i32(1);
        let keys = ["uid", "name", "private", "master", "subdevices"].map(CFString::from_str);
        let values: [&CFType; 5] = [&uid, &name, &private, &member, &members];
        let description = CFDictionary::from_slices(&keys.each_ref().map(|key| &**key), &values);
        let mut id = 0;
        // SAFETY: The retained dictionary and writable ID outlive this call.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(description.as_opaque(), NonNull::from(&mut id))
        };
        assert_eq!(status, 0, "create private CoreAudio aggregate");
        assert_ne!(id, 0);
        Self(Some(id))
    }

    /// Removes the live endpoint immediately.
    ///
    /// # Panics
    /// Panics on a second removal or a CoreAudio failure.
    pub fn remove(&mut self) {
        let id = self.0.expect("owned aggregate");
        // SAFETY: This ID came from our successful, still-live private creation.
        assert_eq!(unsafe { AudioHardwareDestroyAggregateDevice(id) }, 0);
        self.0 = None;
    }
}

impl Drop for Aggregate {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            // SAFETY: Only destroy our own still-live aggregate on early exit.
            unsafe { AudioHardwareDestroyAggregateDevice(id) };
        }
    }
}

/// Returns the active output identifier.
///
/// # Panics
/// Panics if the native host lacks an identifiable default output.
pub fn default_output() -> cpal::DeviceId {
    cpal::default_host()
        .default_output_device()
        .unwrap()
        .id()
        .unwrap()
}

/// Reports whether the endpoint is listed with a usable output format.
pub fn ready(id: &cpal::DeviceId) -> bool {
    cpal::default_host()
        .device_by_id(id)
        .is_some_and(|device| device.default_output_config().is_ok())
}

/// Reports whether the native host has removed the endpoint from enumeration.
pub fn absent(id: &cpal::DeviceId) -> bool {
    cpal::default_host().device_by_id(id).is_none()
}
