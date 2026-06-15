//! No-op host-side mouse + keyboard injection stub for platforms without a real
//! backend (currently macOS).

pub struct DesktopInput;

impl DesktopInput {
	pub fn new() -> std::io::Result<Self> {
		Ok(Self)
	}
	pub fn pointer(&mut self, _x: f64, _y: f64) {}
	pub fn pointer_relative(&mut self, _dx: f64, _dy: f64) {}
	pub fn button(&mut self, _button: u8, _down: bool) {}
	pub fn scroll(&mut self, _dx: f64, _dy: f64) {}
	pub fn key(&mut self, _code: u32, _down: bool) {}
	pub fn type_char(&mut self, _c: char) {}
	pub fn flush_held(&mut self) {}
}
