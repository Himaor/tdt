mod envelope;
pub mod recorder;
pub mod sounds;

pub use envelope::{LiveWave, VIS_BARS};
#[cfg(target_os = "macos")]
pub use recorder::{ask_microphone, microphone_not_asked, take_microphone_answer};
pub use recorder::{watch_default_input, AudioRecorder};
pub use sounds::{play_sound, prewarm, SoundEffect};
