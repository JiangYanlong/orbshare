use mdns_sd::{ServiceDaemon, ServiceInfo};
use orb_core::types::RoomInfo;

pub struct MdnsPublisher {
    mdns: ServiceDaemon,
    service_name: String,
}

impl MdnsPublisher {
    pub fn new() -> Result<Self, String> {
        let mdns = ServiceDaemon::new().map_err(|e| format!("mDNS daemon error: {}", e))?;
        Ok(Self {
            mdns,
            service_name: String::new(),
        })
    }

    pub fn publish(&mut self, room: &RoomInfo) -> Result<(), String> {
        let ty_domain = "_orbshare._udp.local.";
        let instance_name = room.room_id.clone();
        let host_addr = room.host_ip.clone();
        let hostname = format!("{}.local.", room.host_name);
        let port = room.signaling_port;

        let properties: std::collections::HashMap<String, String> = vec![
            ("room_id".to_string(), room.room_id.clone()),
            ("room_name".to_string(), room.room_name.clone()),
            ("host_ip".to_string(), room.host_ip.clone()),
            ("port".to_string(), room.signaling_port.to_string()),
            ("viewer_port".to_string(), room.viewer_port.to_string()),
            ("version".to_string(), room.version.clone()),
            ("max_guests".to_string(), room.max_guests.to_string()),
            (
                "requires_password".to_string(),
                room.requires_password.to_string(),
            ),
            ("capabilities".to_string(), room.capabilities.join(",")),
        ]
        .into_iter()
        .collect();

        let service_info = ServiceInfo::new(
            ty_domain,
            &instance_name,
            &hostname,
            &host_addr,
            port,
            properties,
        )
        .map_err(|e| format!("ServiceInfo error: {}", e))?;

        self.mdns
            .register(service_info)
            .map_err(|e| format!("mDNS register error: {}", e))?;

        self.service_name = instance_name;
        tracing::info!("mDNS published room: {}", room.room_id);
        Ok(())
    }

    pub fn unpublish(&self) -> Result<(), String> {
        if !self.service_name.is_empty() {
            let full_name = format!("{}._orbshare._udp.local.", self.service_name);
            self.mdns
                .unregister(&full_name)
                .map_err(|e| format!("mDNS unregister error: {}", e))?;
            tracing::info!("mDNS unpublished: {}", self.service_name);
        }
        Ok(())
    }
}

impl Drop for MdnsPublisher {
    fn drop(&mut self) {
        let _ = self.unpublish();
    }
}
