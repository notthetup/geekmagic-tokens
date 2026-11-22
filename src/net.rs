use std::{
    error::Error,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Mutex,
        mpsc::Sender,
    },
    thread,
    time::Duration,
};

use esp_idf_svc::{
    eventloop::{EspSubscription, EspSystemEventLoop, System},
    hal::{modem::Modem, reset},
    http::{
        server::{Configuration as HttpConfig, EspHttpConnection, EspHttpServer, Request},
        Method,
    },
    io::Write,
    nvs::EspDefaultNvsPartition,
    ota::EspOta,
    sys,
    netif::IpEvent,
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi, WifiEvent},
};

use crate::{codex, display, ui::Screen};

/// Shared secret for the write endpoints, baked in at build time (see `.env`).
const TOKEN: &str = env!("DEVICE_TOKEN");

/// Boot-time Wi‑Fi join stats for `GET /status`.
static WIFI_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static WIFI_UP_S: AtomicU32 = AtomicU32::new(0);
static WIFI_LAST_ERROR: Mutex<String> = Mutex::new(String::new());

pub fn wifi_summary() -> String {
    format!(
        "wifi: joined at {}s after {} attempt(s){}\n",
        WIFI_UP_S.load(Ordering::Relaxed),
        WIFI_ATTEMPTS.load(Ordering::Relaxed),
        match WIFI_LAST_ERROR.lock().unwrap().as_str() {
            "" => String::new(),
            err => format!(", last failure: {err}"),
        }
    )
}

/// Set by `/debug/drop` to keep the device offline for a while.
static HOLD_OFFLINE: AtomicBool = AtomicBool::new(false);

/// Keeps Wi‑Fi and the OTA server alive for as long as it is held.
pub struct Net {
    _wifi: BlockingWifi<EspWifi<'static>>,
    _server: EspHttpServer<'static>,
    _reconnect: EspSubscription<'static, System>,
    _online: EspSubscription<'static, System>,
}

pub fn start(
    modem: Modem,
    ssid: &str,
    password: &str,
    nvs: EspDefaultNvsPartition,
    wake: Sender<codex::Wake>,
    state: codex::Shared,
) -> Result<Net, Box<dyn Error>> {
    let sysloop = EspSystemEventLoop::take()?;
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))?,
        sysloop.clone(),
    )?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().map_err(|_| "SSID too long")?,
        password: password.try_into().map_err(|_| "password too long")?,
        ..Default::default()
    }))?;
    wifi.start()?;
    // Keep trying until we have an address; giving up would leave the device offline for good.
    loop {
        let attempt = WIFI_ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
        match wifi.connect().and_then(|_| wifi.wait_netif_up()) {
            Ok(()) => break,
            Err(err) => {
                log::warn!("Wi-Fi attempt {attempt} failed: {err}");
                *WIFI_LAST_ERROR.lock().unwrap() = err.to_string();
                let _ = wifi.disconnect();
                thread::sleep(Duration::from_secs(3));
            }
        }
    }
    WIFI_UP_S.store(display::uptime_s(), Ordering::Relaxed);
    let ip = wifi.wifi().sta_netif().get_ip_info()?.ip;
    state.lock().unwrap().ip = Some(ip);

    // ESP-IDF does not rejoin on its own after the access point drops us.
    let reconnect = sysloop.subscribe::<WifiEvent, _>(|event| {
        if matches!(event, WifiEvent::StaDisconnected(_)) && !HOLD_OFFLINE.load(Ordering::Relaxed) {
            log::warn!("Wi-Fi dropped, reconnecting");
            unsafe { sys::esp_wifi_connect() };
        }
    })?;
    // Poll as soon as we are back instead of waiting out the minute.
    let (online_state, online_wake) = (state.clone(), wake.clone());
    let online = sysloop.subscribe::<IpEvent, _>(move |event| {
        if let IpEvent::DhcpIpAssigned(assignment) = event {
            online_state.lock().unwrap().ip = Some(assignment.ip());
            let _ = online_wake.send(codex::Wake::Online);
        }
    })?;

    // `curl --data-binary @app.bin http://<ip>/update` replaces the running firmware.
    let mut server = EspHttpServer::new(&HttpConfig::default())?;
    server.fn_handler("/update", Method::Post, |mut req| -> Result<(), Box<dyn Error>> {
        if !authorized(&req) {
            return deny(req);
        }
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

    let screen_state = state.clone();
    server.fn_handler("/debug/screen.bmp", Method::Get, move |req| -> Result<(), Box<dyn Error>> {
        let screen = if display::SHOWING_CLAUDE.load(Ordering::Relaxed) {
            Screen::Claude
        } else {
            Screen::Codex
        };
        let mut resp = req.into_response(200, None, &[("Content-Type", "image/bmp")])?;
        let state = screen_state.lock().unwrap();
        display::render_bmp(screen, &state, |bytes| resp.write_all(bytes))?;
        Ok(())
    })?;

    let pause_state = state.clone();
    server.fn_handler("/debug/pause", Method::Post, move |req| -> Result<(), Box<dyn Error>> {
        if !authorized(&req) {
            return deny(req);
        }
        let secs = query(req.uri(), "secs");
        let age = query(req.uri(), "age");
        codex::pause(secs);
        if let Some(reading) = &mut pause_state.lock().unwrap().reading {
            reading.at = reading.at.checked_sub(Duration::from_secs(age)).unwrap_or(reading.at);
        }
        req.into_ok_response()?
            .write_all(format!("Polling paused for {secs}s, reading aged {age}s\n").as_bytes())?;
        Ok(())
    })?;

    server.fn_handler("/status", Method::Get, move |req| -> Result<(), Box<dyn Error>> {
        let text = state.lock().unwrap().describe();
        req.into_ok_response()?.write_all(text.as_bytes())?;
        Ok(())
    })?;

    // `POST /debug/drop?secs=N` drops Wi‑Fi and stays offline for N seconds, to test recovery.
    server.fn_handler("/debug/drop", Method::Post, |req| -> Result<(), Box<dyn Error>> {
        if !authorized(&req) {
            return deny(req);
        }
        let secs = query(req.uri(), "secs");
        req.into_ok_response()?
            .write_all(format!("Dropping Wi-Fi for {secs}s\n").as_bytes())?;
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            HOLD_OFFLINE.store(secs > 0, Ordering::Relaxed);
            unsafe { sys::esp_wifi_disconnect() };
            if secs > 0 {
                thread::sleep(Duration::from_secs(secs));
                HOLD_OFFLINE.store(false, Ordering::Relaxed);
                unsafe { sys::esp_wifi_connect() };
            }
        });
        Ok(())
    })?;

    // `curl --data-binary @auth.json http://<ip>/auth` hands the device its own Codex login.
    server.fn_handler("/auth", Method::Post, move |mut req| -> Result<(), Box<dyn Error>> {
        if !authorized(&req) {
            return deny(req);
        }
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
        let _ = wake.send(codex::Wake::NewAuth);
        req.into_ok_response()?.write_all(b"Saved\n")?;
        Ok(())
    })?;

    Ok(Net { _wifi: wifi, _server: server, _reconnect: reconnect, _online: online })
}

fn query(uri: &str, key: &str) -> u64 {
    let query = uri.split_once('?').map_or("", |(_, q)| q);
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
        .unwrap_or(0)
}

fn authorized(req: &Request<&mut EspHttpConnection>) -> bool {
    req.header("X-Device-Token") == Some(TOKEN)
}

fn deny(req: Request<&mut EspHttpConnection>) -> Result<(), Box<dyn Error>> {
    req.into_status_response(401)?.write_all(b"Unauthorized\n")?;
    Ok(())
}

/// Confirms this image boots so a rollback-enabled bootloader keeps it.
pub fn mark_valid() {
    if let Ok(mut ota) = EspOta::new() {
        let _ = ota.mark_running_slot_valid();
    }
}
