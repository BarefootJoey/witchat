//! One-shot position fix for location channels: at launch once the user has
//! turned location on, and whenever they ask.
//! The coordinates are turned into geohashes and never stored or sent.

/// iOS `location_channels.permission_denied`.
pub const DENIED: &str = "location permission denied. enable in settings to use location channels.";

/// Blocking; run off the UI thread. Returns (latitude, longitude).
#[cfg(windows)]
pub fn current_position() -> Result<(f64, f64), String> {
    use std::time::Duration;
    use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy};
    use windows::Foundation::TimeSpan;

    let fail = |e: windows::core::Error| format!("could not get a location fix: {}", e.message());
    let access = Geolocator::RequestAccessAsync().and_then(|op| op.get()).map_err(fail)?;
    if access != GeolocationAccessStatus::Allowed {
        return Err(DENIED.into());
    }
    let locator = Geolocator::new().map_err(fail)?;
    locator.SetDesiredAccuracy(PositionAccuracy::Default).map_err(fail)?;
    let max_age = TimeSpan::from(Duration::from_secs(5 * 60));
    let timeout = TimeSpan::from(Duration::from_secs(20));
    let position = locator
        .GetGeopositionAsyncWithAgeAndTimeout(max_age, timeout)
        .and_then(|op| op.get())
        .and_then(|p| p.Coordinate())
        .and_then(|c| c.Point())
        .and_then(|p| p.Position())
        .map_err(fail)?;
    Ok((position.Latitude, position.Longitude))
}

/// GeoClue2 on the system bus (GNOME, KDE and most desktops ship it). The
/// desktop's location agent decides whether witchat may have a fix.
#[cfg(target_os = "linux")]
pub fn current_position() -> Result<(f64, f64), String> {
    use std::time::{Duration, Instant};
    use zbus::blocking::proxy::Builder;
    use zbus::blocking::{Connection, Proxy};
    use zbus::proxy::CacheProperties;
    use zbus::zvariant::OwnedObjectPath;

    const SERVICE: &str = "org.freedesktop.GeoClue2";
    /// GeoClue `GCLUE_ACCURACY_LEVEL_STREET`: enough for a block-level geohash.
    const STREET: u32 = 6;
    const WAIT: Duration = Duration::from_secs(20);

    let fail = |e: zbus::Error| format!("could not get a location fix: {e}");
    let proxy = |conn: &Connection, path: String, iface: &'static str| -> Result<Proxy<'static>, String> {
        Builder::new(conn)
            .destination(SERVICE)
            .and_then(|b| b.path(path))
            .and_then(|b| b.interface(iface))
            .map(|b| b.cache_properties(CacheProperties::No))
            .and_then(|b| b.build())
            .map_err(fail)
    };
    let conn = Connection::system().map_err(fail)?;
    let manager = proxy(&conn, "/org/freedesktop/GeoClue2/Manager".into(), "org.freedesktop.GeoClue2.Manager")?;
    let client_path: OwnedObjectPath = manager.call("GetClient", &()).map_err(|e| geoclue_error(&e))?;
    let client = proxy(&conn, client_path.to_string(), "org.freedesktop.GeoClue2.Client")?;
    client.set_property("DesktopId", "witchat").map_err(|e| format!("could not get a location fix: {e}"))?;
    client.set_property("RequestedAccuracyLevel", STREET).map_err(|e| format!("could not get a location fix: {e}"))?;
    client.call::<_, _, ()>("Start", &()).map_err(|e| geoclue_error(&e))?;

    let started = Instant::now();
    let fix = loop {
        let path: OwnedObjectPath = client.get_property("Location").map_err(fail)?;
        if path.as_str() != "/" {
            let location = proxy(&conn, path.to_string(), "org.freedesktop.GeoClue2.Location")?;
            let lat: f64 = location.get_property("Latitude").map_err(fail)?;
            let lon: f64 = location.get_property("Longitude").map_err(fail)?;
            break Ok((lat, lon));
        }
        if started.elapsed() > WAIT {
            break Err("could not get a location fix: timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = client.call::<_, _, ()>("Stop", &());
    fix
}

#[cfg(target_os = "linux")]
fn geoclue_error(err: &zbus::Error) -> String {
    let text = err.to_string();
    if text.contains("AccessDenied") || text.contains("not allowed") {
        DENIED.into()
    } else if text.contains("ServiceUnknown") || text.contains("was not provided") {
        "no location service (install and enable geoclue); teleport to a geohash instead".into()
    } else {
        format!("could not get a location fix: {text}")
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn current_position() -> Result<(f64, f64), String> {
    Err("location channels by position need Windows or Linux; teleport to a geohash instead".into())
}

/// Windows Settings > Privacy > Location.
#[cfg(windows)]
pub fn open_location_settings() -> Option<String> {
    std::process::Command::new("explorer")
        .arg("ms-settings:privacy-location")
        .spawn()
        .err()
        .map(|e| format!("could not open settings: {e}"))
}

/// GNOME's location privacy panel; other desktops have no common entry point.
#[cfg(not(windows))]
pub fn open_location_settings() -> Option<String> {
    std::process::Command::new("gnome-control-center")
        .arg("location")
        .spawn()
        .err()
        .map(|_| "open your desktop's privacy settings and allow location for witchat".into())
}

/// The user's measurement system (Region settings), for cell sizes.
#[cfg(windows)]
pub fn uses_metric() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IMEASURE};
    let mut buf = [0u16; 4];
    // LOCALE_IMEASURE: "0" metric, "1" US.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_IMEASURE, Some(&mut buf)) };
    !(n > 0 && buf[0] == u16::from(b'1'))
}

#[cfg(not(windows))]
pub fn uses_metric() -> bool {
    true
}

/// The Windows "Country or region" setting as (ISO code, display name),
/// for iOS quick join. Settings only; no position.
#[cfg(windows)]
pub fn user_region() -> Option<(String, String)> {
    use windows::core::PCWSTR;
    use windows::Win32::Globalization::{GetGeoInfoEx, GetUserDefaultGeoName, GEO_FRIENDLYNAME};
    let mut code = [0u16; 16];
    let n = unsafe { GetUserDefaultGeoName(&mut code) };
    if n <= 1 {
        return None;
    }
    let code_str = String::from_utf16_lossy(&code[..(n as usize - 1)]);
    if code_str.len() != 2 {
        return None;
    }
    let mut name = [0u16; 128];
    let m = unsafe { GetGeoInfoEx(PCWSTR(code.as_ptr()), GEO_FRIENDLYNAME, Some(&mut name)) };
    let name = if m > 1 { String::from_utf16_lossy(&name[..(m as usize - 1)]) } else { code_str.clone() };
    Some((code_str, name))
}

/// The territory of the session locale (`en_GB.UTF-8` → `GB`). Settings only.
#[cfg(not(windows))]
pub fn user_region() -> Option<(String, String)> {
    let locale =
        ["LC_ALL", "LC_ADDRESS", "LANG"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.is_empty())?;
    let code = locale_territory(&locale)?;
    Some((code.clone(), code))
}

#[cfg_attr(windows, allow(dead_code))]
fn locale_territory(locale: &str) -> Option<String> {
    let base = locale.split(['.', '@']).next()?;
    let code = base.split_once('_')?.1;
    (code.len() == 2 && code.chars().all(|c| c.is_ascii_uppercase())).then(|| code.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn region_is_a_two_letter_code() {
        if let Some((code, name)) = super::user_region() {
            assert_eq!(code.len(), 2);
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn locale_territory_reads_posix_locales() {
        assert_eq!(super::locale_territory("en_GB.UTF-8").as_deref(), Some("GB"));
        assert_eq!(super::locale_territory("de_DE@euro").as_deref(), Some("DE"));
        assert_eq!(super::locale_territory("C.UTF-8"), None);
        assert_eq!(super::locale_territory("POSIX"), None);
        assert_eq!(super::locale_territory("es_419.UTF-8"), None);
    }

    /// Touches the real location service: `cargo test -p witchat-app -- --ignored locate`.
    #[test]
    #[ignore]
    fn locate_this_pc() {
        match super::current_position() {
            Ok((lat, lon)) => assert!(lat.abs() <= 90.0 && lon.abs() <= 180.0),
            Err(err) => println!("no fix: {err}"),
        }
    }
}
