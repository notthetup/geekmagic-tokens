use std::{error::Error, net::Ipv4Addr, sync::mpsc::Sender, thread, time::Duration};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{modem::Modem, reset},
    http::{
        server::{Configuration as HttpConfig, EspHttpServer},
        Method,
    },
    io::Write,
    nvs::EspDefaultNvsPartition,
    ota::EspOta,
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};

use crate::codex;

/// Keeps Wi‑Fi and the OTA server alive for as long as it is held.
pub struct Net {
    pub ip: Ipv4Addr,
    _wifi: BlockingWifi<EspWifi<'static>>,
    _server: EspHttpServer<'static>,
}

pub fn start(
    modem: Modem,
    ssid: &str,
    password: &str,
    nvs: EspDefaultNvsPartition,
    wake: Sender<()>,
    state: codex::Shared,
) -> Result<Net, Box<dyn Error>> {
    let sysloop = EspSystemEventLoop::take()?;
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))?,
        sysloop,
    )?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().map_err(|_| "SSID too long")?,
        password: password.try_into().map_err(|_| "password too long")?,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;
    let ip = wifi.wifi().sta_netif().get_ip_info()?.ip;

    // `curl --data-binary @app.bin http://<ip>/update` replaces the running firmware.
    let mut server = EspHttpServer::new(&HttpConfig::default())?;
    server.fn_handler("/update", Method::Post, |mut req| -> Result<(), Box<dyn Error>> {
        let mut ota = EspOta::new()?;
        let mut update = ota.initiate_update()?;
        let mut buf = vec![0; 4096];
        let mut total = 0;
        loop {
            let n = req.read(&mut buf)?;
            if n == 0 {
                break;
            }
            update.write_all(&buf[..n])?;
            total += n;
        }
        update.complete()?;
        req.into_ok_response()?
            .write_all(format!("Wrote {total} bytes, restarting\n").as_bytes())?;
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(1));
            reset::restart();
        });
        Ok(())
    })?;

    server.fn_handler("/status", Method::Get, move |req| -> Result<(), Box<dyn Error>> {
        let text = state.lock().unwrap().describe();
        req.into_ok_response()?.write_all(text.as_bytes())?;
        Ok(())
    })?;

    // `curl --data-binary @auth.json http://<ip>/auth` hands the device its own Codex login.
    server.fn_handler("/auth", Method::Post, move |mut req| -> Result<(), Box<dyn Error>> {
        let mut body = Vec::new();
        let mut buf = [0; 512];
        loop {
            let n = req.read(&mut buf)?;
            if n == 0 || body.len() > 16 * 1024 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        codex::save_auth(&nvs, &body)?;
        let _ = wake.send(());
        req.into_ok_response()?.write_all(b"Saved\n")?;
        Ok(())
    })?;

    Ok(Net { ip, _wifi: wifi, _server: server })
}

/// Confirms this image boots so a rollback-enabled bootloader keeps it.
pub fn mark_valid() {
    if let Ok(mut ota) = EspOta::new() {
        let _ = ota.mark_running_slot_valid();
    }
}
