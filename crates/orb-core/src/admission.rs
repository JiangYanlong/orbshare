use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Conservative default used by callers that do not choose a room limit.
pub const DEFAULT_MAX_GUESTS: u32 = 10;
/// Room-wide safety limit for the current peer-to-peer architecture.
pub const EXTENDED_MAX_GUESTS: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    Admitted,
    AlreadyAdmitted,
    Full,
}

/// One room-wide capacity gate shared by native and browser join paths.
///
/// The critical sections only insert/remove a short identifier, so a standard
/// mutex avoids requiring an async lock at every signaling call site.
#[derive(Debug, Clone)]
pub struct RoomAdmission {
    max_guests: usize,
    admitted: Arc<Mutex<HashSet<String>>>,
}

impl RoomAdmission {
    pub fn new(max_guests: u32) -> Self {
        Self {
            max_guests: max_guests.max(1) as usize,
            admitted: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn try_admit(&self, guest_id: &str) -> AdmissionDecision {
        let mut admitted = self.admitted.lock().unwrap_or_else(|e| e.into_inner());
        if admitted.contains(guest_id) {
            return AdmissionDecision::AlreadyAdmitted;
        }
        if admitted.len() >= self.max_guests {
            return AdmissionDecision::Full;
        }
        admitted.insert(guest_id.to_string());
        AdmissionDecision::Admitted
    }

    pub fn release(&self, guest_id: &str) -> bool {
        self.admitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(guest_id)
    }

    pub fn clear(&self) {
        self.admitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn current_guests(&self) -> usize {
        self.admitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn max_guests(&self) -> u32 {
        self.max_guests as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_capacity_across_clones_and_allows_reconnect_identity() {
        let admission = RoomAdmission::new(2);
        let other_path = admission.clone();

        assert_eq!(admission.try_admit("native-1"), AdmissionDecision::Admitted);
        assert_eq!(
            other_path.try_admit("browser-1"),
            AdmissionDecision::Admitted
        );
        assert_eq!(
            admission.try_admit("browser-1"),
            AdmissionDecision::AlreadyAdmitted
        );
        assert_eq!(other_path.try_admit("guest-3"), AdmissionDecision::Full);
        assert_eq!(admission.current_guests(), 2);

        assert!(other_path.release("native-1"));
        assert_eq!(admission.try_admit("guest-3"), AdmissionDecision::Admitted);
    }

    #[test]
    fn never_creates_a_zero_capacity_room() {
        let admission = RoomAdmission::new(0);
        assert_eq!(admission.max_guests(), 1);
        assert_eq!(admission.try_admit("guest"), AdmissionDecision::Admitted);
    }
}
