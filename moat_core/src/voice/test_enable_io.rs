use coreaudio::audio_unit::{AudioUnit, IOType, Scope, Element};
fn main() {
    let mut au = AudioUnit::new(IOType::VoiceProcessingIO).unwrap();
    // au.set_property
}
