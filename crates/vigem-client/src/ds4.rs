use std::{fmt, mem, ptr};
use std::borrow::Borrow;
use crate::*;
#[cfg(feature = "unstable_ds4")]
use winapi::shared::ntdef::HANDLE;

/// DualShock4 HID Input report.
#[cfg(feature = "unstable_ds4")]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(C)]
pub struct DS4Report {
	pub thumb_lx: u8,
	pub thumb_ly: u8,
	pub thumb_rx: u8,
	pub thumb_ry: u8,
	pub buttons: u16,
	pub special: u8,
	pub trigger_l: u8,
	pub trigger_r: u8,
}
#[cfg(feature = "unstable_ds4")]
impl Default for DS4Report {
	#[inline]
	fn default() -> Self {
		DS4Report {
			thumb_lx: 0x80,
			thumb_ly: 0x80,
			thumb_rx: 0x80,
			thumb_ry: 0x80,
			buttons: 0x8,
			special: 0,
			trigger_l: 0,
			trigger_r: 0,
		}
	}
}

/// One DualShock4 touch frame (9 bytes). `is_up_tracking_*` high bit (0x80) = finger up.
/// PACKED + no Debug/Eq derives (those take references to fields, illegal on packed).
#[cfg(feature = "unstable_ds4")]
#[derive(Copy, Clone)]
#[repr(C, packed)]
pub struct DS4Touch {
	pub packet_counter: u8,
	pub is_up_tracking_num1: u8,
	pub touch_data1: [u8; 3],
	pub is_up_tracking_num2: u8,
	pub touch_data2: [u8; 3],
}
#[cfg(feature = "unstable_ds4")]
impl Default for DS4Touch {
	#[inline]
	fn default() -> Self {
		// "No touch": both finger slots flagged up (0x80), per ViGEm/Sunshine baseline.
		DS4Touch {
			packet_counter: 0,
			is_up_tracking_num1: 0x80,
			touch_data1: [0, 0, 0],
			is_up_tracking_num2: 0x80,
			touch_data2: [0, 0, 0],
		}
	}
}

/// DualShock4 v1 complete HID input report — the 63-byte `DS4_REPORT_EX` ViGEmBus expects
/// via `IOCTL_DS4_SUBMIT_REPORT_EX`. PACKED (the HID report has no alignment padding, e.g.
/// `timestamp` sits at odd offset 9) + tail-padded to ViGEm's 63-byte `ReportBuffer`.
/// Unlike the legacy `DS4Report`, this surfaces the PS button (`special` bit 0) + touchpad
/// to Windows/Steam — see ViGEm Ds4Pdo.cpp.
#[cfg(feature = "unstable_ds4")]
#[derive(Copy, Clone)]
#[repr(C, packed)]
pub struct DS4ReportEx {
	pub thumb_lx: u8,
	pub thumb_ly: u8,
	pub thumb_rx: u8,
	pub thumb_ry: u8,
	pub buttons: u16,
	pub special: u8,
	pub trigger_l: u8,
	pub trigger_r: u8,
	pub timestamp: u16,
	pub battery_lvl: u8,
	pub gyro_x: i16,
	pub gyro_y: i16,
	pub gyro_z: i16,
	pub accel_x: i16,
	pub accel_y: i16,
	pub accel_z: i16,
	pub _unknown1: [u8; 5],
	pub battery_lvl_special: u8,
	pub _unknown2: [u8; 2],
	pub touch_packets_n: u8, // 0x00 to 0x03 (USB max)
	pub current_touch: DS4Touch,
	pub previous_touch: [DS4Touch; 2],
	/// Trailing pad so `size_of::<DS4ReportEx>() == 63` (ViGEm `ReportBuffer[63]`).
	pub _pad: [u8; 3],
}
#[cfg(feature = "unstable_ds4")]
impl Default for DS4ReportEx {
	#[inline]
	fn default() -> Self {
		DS4ReportEx {
			thumb_lx: 0x80,
			thumb_ly: 0x80,
			thumb_rx: 0x80,
			thumb_ry: 0x80,
			buttons: 0x8, // DS4_BUTTON_DPAD_NONE
			special: 0,
			trigger_l: 0,
			trigger_r: 0,
			timestamp: 0,
			battery_lvl: 0xFF,
			gyro_x: 0,
			gyro_y: 0,
			gyro_z: 0,
			accel_x: 0,
			accel_y: 0,
			accel_z: 0,
			_unknown1: [0; 5],
			battery_lvl_special: 0x1A, // Wired - full battery
			_unknown2: [0; 2],
			touch_packets_n: 1,
			current_touch: DS4Touch::new_unused(),
			previous_touch: [DS4Touch::new_unused(), DS4Touch::new_unused()],
			_pad: [0; 3],
		}
	}
}
// The HID report must be EXACTLY 63 bytes (ViGEm `ReportBuffer[63]`); a wrong size makes
// the EX IOCTL fail. Catch any layout drift at compile time.
#[cfg(feature = "unstable_ds4")]
const _: () = assert!(mem::size_of::<DS4ReportEx>() == 63);
#[cfg(feature = "unstable_ds4")]
const _: () = assert!(mem::size_of::<DS4Touch>() == 9);

#[cfg(feature = "unstable_ds4")]
impl DS4Touch {
	#[inline]
	const fn new_unused() -> Self {
		DS4Touch {
			packet_counter: 0,
			is_up_tracking_num1: 0x80,
			touch_data1: [0, 0, 0],
			is_up_tracking_num2: 0x80,
			touch_data2: [0, 0, 0],
		}
	}
}

/// A virtual Sony DualShock 4 (wired).
pub struct DualShock4Wired<CL: Borrow<Client>> {
	client: CL,
	event: Event,
	serial_no: u32,
	id: TargetId,
}

impl<CL: Borrow<Client>> DualShock4Wired<CL> {
	/// Creates a new instance.
	#[inline]
	pub fn new(client: CL, id: TargetId) -> DualShock4Wired<CL> {
		let event = Event::new(false, false);
		DualShock4Wired { client, event, serial_no: 0, id }
	}

	/// Returns if the controller is plugged in.
	#[inline]
	pub fn is_attached(&self) -> bool {
		self.serial_no != 0
	}

	/// Returns the id the controller was constructed with.
	#[inline]
	pub fn id(&self) -> TargetId {
		self.id
	}

	/// Returns the client.
	#[inline]
	pub fn client(&self) -> &CL {
		&self.client
	}

	/// Unplugs and destroys the controller, returning the client.
	#[inline]
	pub fn drop(mut self) -> CL {
		let _ = self.unplug();

		unsafe {
			let client = (&self.client as *const CL).read();
			ptr::drop_in_place(&mut self.event);
			mem::forget(self);
			client
		}
	}

	/// Plugs the controller in.
	#[inline(never)]
	pub fn plugin(&mut self) -> Result<(), Error> {
		if self.is_attached() {
			return Err(Error::AlreadyConnected);
		}

		self.serial_no = unsafe {
			let mut plugin = bus::PluginTarget::ds4_wired(1, self.id.vendor, self.id.product);
			let device = self.client.borrow().device;

			// Yes this is how the driver is implemented
			while plugin.ioctl(device, self.event.handle).is_err() {
				plugin.SerialNo += 1;
				if plugin.SerialNo >= u16::MAX as u32 {
					return Err(Error::NoFreeSlot);
				}
			}

			plugin.SerialNo
		};

		Ok(())
	}

	/// Unplugs the controller.
	#[inline(never)]
	pub fn unplug(&mut self) -> Result<(), Error> {
		if !self.is_attached() {
			return Err(Error::NotPluggedIn);
		}

		unsafe {
			let mut unplug = bus::UnplugTarget::new(self.serial_no);
			let device = self.client.borrow().device;
			unplug.ioctl(device, self.event.handle)?;
		}

		self.serial_no = 0;
		Ok(())
	}

	/// Waits until the virtual controller is ready.
	///
	/// Any updates submitted before the virtual controller is ready may return an error.
	#[inline(never)]
	pub fn wait_ready(&mut self) -> Result<(), Error> {
		if !self.is_attached() {
			return Err(Error::NotPluggedIn);
		}

		unsafe {
			let mut wait = bus::WaitDeviceReady::new(self.serial_no);
			let device = self.client.borrow().device;
			wait.ioctl(device, self.event.handle)?;
		}

		Ok(())
	}

	/// Updates the virtual controller state.
	#[cfg(feature = "unstable_ds4")]
	#[inline(never)]
	pub fn update(&mut self, report: &DS4Report) -> Result<(), Error> {
		if !self.is_attached() {
			return Err(Error::NotPluggedIn);
		}

		unsafe {
			let mut dsr = bus::DS4SubmitReport::new(self.serial_no, *report);
			let device = self.client.borrow().device;
			dsr.ioctl(device, self.event.handle)?;
		}

		Ok(())
	}

	/// Updates the virtual controller with a complete 63-byte `DS4_REPORT_EX` (the path
	/// that surfaces the PS button + touchpad to Windows/Steam). Use this over `update`.
	#[cfg(feature = "unstable_ds4")]
	#[inline(never)]
	pub fn update_ex(&mut self, report: &DS4ReportEx) -> Result<(), Error> {
		if !self.is_attached() {
			return Err(Error::NotPluggedIn);
		}

		unsafe {
			let mut dsr = bus::DS4SubmitReportEx::new(self.serial_no, *report);
			let device = self.client.borrow().device;
			dsr.ioctl(device, self.event.handle)?;
		}

		Ok(())
	}

	/// Returns a [`Ds4Notifier`] for reading rumble/LED feedback the consuming app sends to
	/// this virtual DS4. It owns a copy of the device handle + serial + its own event, so it
	/// can be moved to a dedicated thread that blocks in [`Ds4Notifier::await_notification`].
	#[cfg(feature = "unstable_ds4")]
	#[inline]
	pub fn notifier(&self) -> Ds4Notifier {
		Ds4Notifier {
			device: self.client.borrow().device,
			serial_no: self.serial_no,
			event: Event::new(false, false),
		}
	}
}

/// One rumble/LED notification the consuming app sent to a virtual DS4.
#[cfg(feature = "unstable_ds4")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Ds4Notification {
	/// Heavy / strong (left) motor magnitude, 0–255.
	pub large_motor: u8,
	/// Light / weak (right) motor magnitude, 0–255.
	pub small_motor: u8,
	/// Lightbar color (R, G, B).
	pub lightbar: [u8; 3],
}

/// Reads rumble/LED notifications for a virtual DS4 from a dedicated thread (the device
/// handle is shared with the submitting side; each side uses its own OVERLAPPED + event).
#[cfg(feature = "unstable_ds4")]
pub struct Ds4Notifier {
	device: HANDLE,
	serial_no: u32,
	event: Event,
}
// Safety: HANDLE is a kernel handle valid across threads; the Event is Send. The submitting
// side and this notifier use independent OVERLAPPED structures, so concurrent use is sound.
#[cfg(feature = "unstable_ds4")]
unsafe impl Send for Ds4Notifier {}

#[cfg(feature = "unstable_ds4")]
impl Ds4Notifier {
	/// Blocks until the consuming app sends an output report (rumble/lightbar) to the
	/// virtual pad, returning the motor magnitudes + lightbar color. Errors when the target
	/// is unplugged (the caller should stop the loop then).
	#[inline(never)]
	pub fn await_notification(&mut self) -> Result<Ds4Notification, Error> {
		if self.serial_no == 0 {
			return Err(Error::NotPluggedIn);
		}
		unsafe {
			let mut n = bus::Ds4AwaitNotification::new(self.serial_no);
			n.ioctl(self.device, self.event.handle)?;
			Ok(Ds4Notification {
				large_motor: n.LargeMotor,
				small_motor: n.SmallMotor,
				lightbar: [n.LightbarR, n.LightbarG, n.LightbarB],
			})
		}
	}
}

impl<CL: Borrow<Client>> fmt::Debug for DualShock4Wired<CL> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("DualShock4Wired")
			.field("serial_no", &self.serial_no)
			.field("vendor_id", &self.id.vendor)
			.field("product_id", &self.id.product)
			.finish()
	}
}

impl<CL: Borrow<Client>> Drop for DualShock4Wired<CL> {
	#[inline]
	fn drop(&mut self) {
		let _ = self.unplug();
	}
}
