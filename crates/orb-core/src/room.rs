use crate::types::{MemberInfo, RoomInfo};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Hash a normalized password before it is sent over the LAN.
///
/// The returned value is still a password equivalent, but it avoids putting the
/// user's plain-text password directly in signaling payloads.
pub fn hash_password_for_transport(password: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Build the salted verifier stored by the host.
pub fn hash_password_for_storage(transport_hash: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(transport_hash.as_bytes());
    hasher.update(salt.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Verify a transport hash without exposing the host's plain-text password.
pub fn verify_password_hash(transport_hash: &str, stored_hash: &str, salt: &str) -> bool {
    stored_hash.is_empty() || hash_password_for_storage(transport_hash, salt) == stored_hash
}

/// Normalize a password value.
/// Returns None if the password is effectively empty (no password required).
/// Returns Some(trimmed) if the password is non-empty after trimming.
pub fn normalize_password(password: &str) -> Option<String> {
    let trimmed = password.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub struct RoomManager {
    pub room: Option<RoomInfo>,
    pub members: Arc<RwLock<HashMap<String, MemberInfo>>>,
    pub password_hash: Option<String>,
    pub password_salt: String,
}

impl RoomManager {
    pub fn new() -> Self {
        Self {
            room: None,
            members: Arc::new(RwLock::new(HashMap::new())),
            password_hash: None,
            password_salt: uuid_simple(),
        }
    }

    /// Set the room password. If password is empty/whitespace, no password is required.
    pub fn set_password(&mut self, password: &str) {
        match normalize_password(password) {
            Some(pw) => {
                let transport_hash = hash_password_for_transport(&pw);
                self.password_hash = Some(hash_password_for_storage(
                    &transport_hash,
                    &self.password_salt,
                ));
            }
            None => {
                self.password_hash = None;
            }
        }
    }

    /// Returns true if a password is required for this room.
    pub fn requires_password(&self) -> bool {
        self.password_hash.is_some()
    }

    pub fn verify_password(&self, password: &str) -> bool {
        let Some(ref stored_hash) = self.password_hash else {
            return true; // no password set
        };
        let Some(normalized) = normalize_password(password) else {
            return false;
        };
        let transport_hash = hash_password_for_transport(&normalized);
        verify_password_hash(&transport_hash, stored_hash, &self.password_salt)
    }
}

impl Default for RoomManager {
    fn default() -> Self {
        Self::new()
    }
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_protocol_matches_transport_and_storage_hashes() {
        let mut manager = RoomManager::new();
        manager.password_salt = "test-salt".to_string();
        manager.set_password("  secret  ");

        let transport_hash = hash_password_for_transport("secret");
        assert!(verify_password_hash(
            &transport_hash,
            manager.password_hash.as_deref().unwrap(),
            &manager.password_salt,
        ));
        assert!(manager.verify_password(" secret "));
        assert!(!manager.verify_password("wrong"));
    }

    #[test]
    fn blank_password_disables_authentication() {
        let mut manager = RoomManager::new();
        manager.set_password("   ");

        assert!(!manager.requires_password());
        assert!(manager.verify_password("anything"));
    }
}
