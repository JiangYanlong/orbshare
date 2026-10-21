use mdns_sd::{ServiceDaemon, ServiceEvent};
use orb_core::types::RoomInfo;
use tokio::sync::mpsc;

pub struct MdnsScanner {
    mdns: ServiceDaemon,
}

impl MdnsScanner {
    pub fn new() -> Result<Self, String> {
        let mdns = ServiceDaemon::new().map_err(|e| format!("mDNS daemon error: {}", e))?;
        Ok(Self { mdns })
    }

    pub fn start_scan(&self) -> Result<mpsc::UnboundedReceiver<ScannerEvent>, String> {
        let receiver = self
            .mdns
            .browse("_orbshare._udp.local.")
            .map_err(|e| format!("mDNS browse error: {}", e))?;

        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let props = info.get_properties();
                        let room = RoomInfo {
                            room_id: props
                                .get("room_id")
                                .map(|v| v.val_str().to_string())
                                .unwrap_or_default(),
                            room_name: props
                                .get("room_name")
                                .map(|v| v.val_str().to_string())
                                .unwrap_or_default(),
                            host_name: info.get_hostname().to_string(),
                            host_ip: props
                                .get("host_ip")
                                .map(|v| v.val_str().to_string())
                                .unwrap_or_default(),
                            signaling_port: props
                                .get("port")
                                .and_then(|v| v.val_str().parse().ok())
                                .unwrap_or(0),
                            viewer_port: props
                                .get("viewer_port")
                                .and_then(|v| v.val_str().parse().ok())
                                .unwrap_or(0),
                            version: props
                                .get("version")
                                .map(|v| v.val_str().to_string())
                                .unwrap_or_default(),
                            max_guests: props
                                .get("max_guests")
                                .and_then(|v| v.val_str().parse().ok())
                                .unwrap_or(5),
                            current_guests: 0,
                            capabilities: props
                                .get("capabilities")
                                .map(|v| v.val_str().split(',').map(|s| s.to_string()).collect())
                                .unwrap_or_default(),
                            requires_password: props
                                .get("requires_password")
                                .map(|v| v.val_str() == "true")
                                .unwrap_or(false),
                        };
                        let _ = tx.send(ScannerEvent::RoomDiscovered(room));
                    }
                    ServiceEvent::ServiceRemoved(_, name) => {
                        let _ = tx.send(ScannerEvent::RoomRemoved(name));
                    }
                    _ => {}
                }
            }
        });

        Ok(rx)
    }
}

#[derive(Debug)]
pub enum ScannerEvent {
    RoomDiscovered(RoomInfo),
    RoomRemoved(String),
}
