//! The identification switch (spec §15.3, D-111): the admin setting `autoEnrichMetadata`
//! combined with `KIMA_IDENTIFY=off`, which wins over it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One shared flag: clones see each other's `set_setting`.
#[derive(Debug, Clone)]
pub struct IdentifySwitch {
    env_off: bool,
    setting: Arc<AtomicBool>,
}

impl IdentifySwitch {
    pub fn new(env_off: bool, setting: bool) -> Self {
        Self {
            env_off,
            setting: Arc::new(AtomicBool::new(setting)),
        }
    }

    /// Whether outbound lookups may run.
    pub fn enabled(&self) -> bool {
        self.setting.load(Ordering::Relaxed) && !self.env_off
    }

    pub fn set_setting(&self, on: bool) {
        self.setting.store(on, Ordering::Relaxed);
    }

    /// True when `KIMA_IDENTIFY=off` overrides the setting.
    pub fn locked(&self) -> bool {
        self.env_off
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_is_setting_and_not_env_off() {
        assert!(IdentifySwitch::new(false, true).enabled());
        assert!(!IdentifySwitch::new(false, false).enabled());
        assert!(!IdentifySwitch::new(true, true).enabled());
        assert!(!IdentifySwitch::new(true, false).enabled());
    }

    #[test]
    fn env_off_locks_and_setting_still_stored() {
        let switch = IdentifySwitch::new(true, true);
        assert!(switch.locked());
        switch.set_setting(true);
        assert!(!switch.enabled());
        assert!(!IdentifySwitch::new(false, true).locked());
    }

    #[test]
    fn clones_share_the_setting() {
        let a = IdentifySwitch::new(false, true);
        let b = a.clone();
        b.set_setting(false);
        assert!(!a.enabled());
        a.set_setting(true);
        assert!(b.enabled());
    }
}
