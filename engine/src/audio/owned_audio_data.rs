//! RAII owner for [`web_sys::AudioData`] — `close()` on drop.

use std::mem::ManuallyDrop;
use std::ops::Deref;

use web_sys::AudioData;

/// Owned WebCodecs audio sample; closes the browser resource on drop.
pub struct OwnedAudioData(AudioData);

impl OwnedAudioData {
    pub fn new(data: AudioData) -> Self {
        Self(data)
    }

    pub fn as_web_sys(&self) -> &AudioData {
        &self.0
    }

    /// Move out the raw handle without closing. Caller must `.close()`.
    pub fn into_raw(self) -> AudioData {
        let this = ManuallyDrop::new(self);
        unsafe { std::ptr::read(&this.0) }
    }
}

impl Deref for OwnedAudioData {
    type Target = AudioData;

    fn deref(&self) -> &AudioData {
        &self.0
    }
}

impl Drop for OwnedAudioData {
    fn drop(&mut self) {
        self.0.close();
    }
}
