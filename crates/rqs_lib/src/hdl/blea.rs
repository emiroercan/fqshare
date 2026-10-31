use std::sync::Arc;

use bluer::UuidExt;
use bluer::adv::Advertisement;
use bytes::Bytes;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const SERVICE_DATA: Bytes = Bytes::from_static(&[
    252, 18, 142, 1, 66, 0, 0, 0, 0, 0, 0, 0, 0, 0, 191, 45, 91, 160, 225, 216, 117, 36, 202, 0,
]);

const INNER_NAME: &str = "BleAdvertiser";

#[derive(Debug, Clone)]
pub struct BleAdvertiser {
    adapter: Arc<bluer::Adapter>,
}

impl BleAdvertiser {
    pub async fn new() -> Result<Self, anyhow::Error> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;

        Ok(Self {
            adapter: Arc::new(adapter),
        })
    }

    pub async fn run(&self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!(
            "{INNER_NAME}: advertising on Bluetooth adapter {} with address {}",
            self.adapter.name(),
            self.adapter.address().await?
        );

        let service_uuid = Uuid::from_u16(0xFE2C);
        let handle = self
            .adapter
            .advertise(self.get_advertisement(service_uuid, SERVICE_DATA))
            .await?;
        ctk.cancelled().await;
        info!("{INNER_NAME}: tracker cancelled, returning");
        drop(handle);

        Ok(())
    }

    fn get_advertisement(&self, service_uuid: Uuid, adv_data: Bytes) -> Advertisement {
        Advertisement {
            advertisement_type: bluer::adv::Type::Broadcast,
            service_data: [(service_uuid, adv_data.into())].into(),
            ..Default::default()
        }
    }
}

// ----------------------------------------------------------------------------
// QuickShare *receiver* discovery over BLE (service UUID 0xFEF3).
//
// Unlike `BleAdvertiser` (which emits the 0xFE2C "I want to send" FastInit
// beacon during the discovery/send phase), this advertises the device as a
// discoverable Nearby Connections *endpoint* so a phone that has dropped off
// Wi-Fi during its browse phase (the Pixel "AirDrop update" behaviour) can
// still list us as a target. Once the user selects us, the phone reconnects to
// Wi-Fi and the transfer completes over the normal Wi-Fi-LAN (mDNS + TCP) path,
// which is why the advertised endpoint_id MUST match the one used by MDnsServer.
//
// The byte layout was reverse-engineered from a live capture of a Pixel 9 Pro
// advertising in "Everyone" mode. The identity/MAC/trailing-presence segments
// are reused verbatim from that capture (they are not validated for discovery);
// only the endpoint_id, device name and the length fields vary.
const RX_INNER_NAME: &str = "ReceiverAdvertiser";
const QS_SERVICE_UUID: u16 = 0xFEF3;
// 3-byte hash of the "NearbySharing" service id (matches the mDNS _FC9F5ED42C8A type).
const QS_SVC_HASH: [u8; 3] = [0xfc, 0x9f, 0x5e];
/// Build the 0xFEF3 service data advertising this device as a QuickShare
/// receiver endpoint (google/nearby `mediums/ble/ble_advertisement.cc` wrapping
/// `connections/implementation/ble_advertisement.cc`, regular, not fast).
///
/// The Bluetooth Classic MAC is left empty on purpose: when it is set the phone
/// first tries RFCOMM, which on Linux needs a pairing prompt, and only falls
/// back to the BLE GATT socket ~20 s later. With no MAC it goes straight to GATT.
pub fn receiver_service_data(endpoint_id: [u8; 4], device_type: u8, device_name: &str) -> Vec<u8> {
    // Nearby Share endpoint info: header, 16 random bytes (salt + metadata key
    // hash, unused for "Everyone" visibility), plaintext name.
    let mut name = device_name.as_bytes().to_vec();
    name.truncate(100);
    let mut einfo = crate::utils::gen_endpoint_info(device_type, "");
    einfo.pop(); // drop the empty name's length byte
    einfo[0] |= 1 << 5; // version 1
    einfo.push(name.len() as u8);
    einfo.extend_from_slice(&name);

    // Connections-layer advertisement.
    let mut data: Vec<u8> = Vec::new();
    data.push(0x23); // version(3b)=1 | pcp(5b)=P2P_POINT_TO_POINT
    data.extend_from_slice(&QS_SVC_HASH);
    data.extend_from_slice(&endpoint_id);
    data.push(einfo.len() as u8);
    data.extend_from_slice(&einfo);
    data.extend_from_slice(&[0u8; 6]); // no Bluetooth Classic MAC
    data.push(0); // uwb address length
    data.push(0); // extra field (webrtc not connectable)

    // Mediums-layer wrapper.
    let mut sd: Vec<u8> = Vec::new();
    sd.push(0x48); // version(3b)=2 | socket_version(3b)=2 | fast(1b)=0 | reserved
    sd.extend_from_slice(&QS_SVC_HASH);
    sd.extend_from_slice(&(data.len() as u32).to_be_bytes());
    sd.extend_from_slice(&data);
    let token: [u8; 2] = rand::random();
    sd.extend_from_slice(&token); // device token
    sd
}

#[derive(Debug, Clone)]
pub struct ReceiverAdvertiser {
    adapter: Arc<bluer::Adapter>,
    service_data: Vec<u8>,
}

impl ReceiverAdvertiser {
    /// `service_data` comes from `receiver_service_data` and must be the same
    /// bytes served on GATT slot 0.
    pub async fn new(service_data: Vec<u8>) -> Result<Self, anyhow::Error> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;

        Ok(Self {
            adapter: Arc::new(adapter),
            service_data,
        })
    }

    pub async fn run(&self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!(
            "{RX_INNER_NAME}: advertising QuickShare receiver (0x{QS_SERVICE_UUID:04X}, {} bytes) on adapter {} ({})",
            self.service_data.len(),
            self.adapter.name(),
            self.adapter.address().await?
        );

        let uuid = Uuid::from_u16(QS_SERVICE_UUID);
        // A connectable advertising set is consumed when a phone connects, so
        // re-register periodically to stay discoverable for further transfers.
        loop {
            let adv = Advertisement {
                // Connectable, matching how the phone advertises as a receiver.
                advertisement_type: bluer::adv::Type::Peripheral,
                service_data: [(uuid, self.service_data.clone())].into(),
                discoverable: Some(true),
                // Advertise fast so the phone discovers and connects quickly
                // (BlueZ defaults to a slow ~1-2s interval).
                min_interval: Some(std::time::Duration::from_millis(100)),
                max_interval: Some(std::time::Duration::from_millis(150)),
                ..Default::default()
            };
            match self.adapter.advertise(adv).await {
                Ok(handle) => {
                    tokio::select! {
                        _ = ctk.cancelled() => {
                            drop(handle);
                            info!("{RX_INNER_NAME}: tracker cancelled, returning");
                            return Ok(());
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                            drop(handle);
                        }
                    }
                }
                Err(e) => {
                    warn!("{RX_INNER_NAME}: advertise failed ({e}); retrying");
                    tokio::select! {
                        _ = ctk.cancelled() => return Ok(()),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
                    }
                }
            }
        }
    }
}
