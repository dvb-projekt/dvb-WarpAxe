//! Wi-Fi station, optional: only when the config has an SSID. Runs on its
//! own thread with a reconnect loop; the rest of the firmware sees it as
//! one more interface in `net::state()`.

use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

pub fn start(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: String,
    pass: String,
) {
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || {
            let mut wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs)) {
                Ok(w) => w,
                Err(e) => {
                    log::error!("wifi: init failed: {e}");
                    return;
                }
            };
            let cfg = Configuration::Client(ClientConfiguration {
                ssid: heapless::String::try_from(ssid.as_str()).unwrap_or_default(),
                password: heapless::String::try_from(pass.as_str()).unwrap_or_default(),
                auth_method: if pass.is_empty() {
                    AuthMethod::None
                } else {
                    AuthMethod::WPA2Personal
                },
                ..Default::default()
            });
            if let Err(e) = wifi.set_configuration(&cfg) {
                log::error!("wifi: config failed: {e}");
                return;
            }
            let mut wifi = match BlockingWifi::wrap(&mut wifi, sysloop) {
                Ok(w) => w,
                Err(e) => {
                    log::error!("wifi: wrap failed: {e}");
                    return;
                }
            };
            loop {
                let up = wifi
                    .start()
                    .and_then(|_| wifi.connect())
                    .and_then(|_| wifi.wait_netif_up());
                match up {
                    Ok(()) => {
                        let ip = wifi
                            .wifi()
                            .sta_netif()
                            .get_ip_info()
                            .map(|i| i.ip.to_string())
                            .ok();
                        log::info!("wifi: connected, ip {ip:?}");
                        super::set_wifi(true, ip, None);
                        while wifi.is_connected().unwrap_or(false) {
                            std::thread::sleep(Duration::from_secs(2));
                        }
                        log::warn!("wifi: link lost");
                        super::set_wifi(false, None, None);
                    }
                    Err(e) => {
                        log::warn!("wifi: connect failed: {e}");
                        super::set_wifi(false, None, None);
                        let _ = wifi.stop();
                        std::thread::sleep(Duration::from_secs(10));
                    }
                }
            }
        })
        .expect("wifi thread");
}
