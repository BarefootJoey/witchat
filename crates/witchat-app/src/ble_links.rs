//! `witchat --ble-links`: does Windows still hold a connection to the
//! phones witchat used recently? Run it with witchat closed. A phone still
//! "connected" then is the leftover connection that keeps phones from
//! reappearing until Bluetooth is turned off and on.

use anyhow::Result;

/// Phones in the newest log, most recent last.
const KEEP: usize = 12;

pub fn run() -> Result<()> {
    let Some(log) = crate::logging::files_newest_first().into_iter().next() else {
        println!("no witchat log yet, so no phones to check");
        return Ok(());
    };
    let phones = recent_phones(&std::fs::read_to_string(&log)?);
    if phones.is_empty() {
        println!("no phones in {}", log.display());
        return Ok(());
    }
    if !cfg!(windows) {
        println!("only Windows reports this; BlueZ drops a closed program's connections itself");
        return Ok(());
    }
    let mut held = 0;
    for phone in &phones {
        let state = match witchat_ble::radio::os_connected(phone) {
            Some(true) => {
                held += 1;
                "CONNECTED"
            }
            Some(false) => "not connected",
            None => "unknown to Windows",
        };
        println!("{phone}  {state}");
    }
    if crate::instance::is_running() {
        println!("\nwitchat is running, so connected phones are its live links; quit it first to check for leftovers");
    } else if held > 0 {
        println!(
            "\n{held} still connected. With witchat closed, that connection is left over; turn Bluetooth off and \
             on (or press \"reset Bluetooth\" in witchat) before starting it again."
        );
    } else {
        println!("\nnone held; Bluetooth is clean for the next start");
    }
    Ok(())
}

/// Phone addresses (`AA:BB:CC:DD:EE:FF`) from `id=` fields, deduplicated.
fn recent_phones(log: &str) -> Vec<String> {
    let mut phones: Vec<String> = Vec::new();
    for line in log.lines() {
        for field in line.split_whitespace().filter_map(|w| w.strip_prefix("id=")) {
            let addr = field.trim_start_matches("in:");
            let is_address = addr.len() == 17
                && addr.split(':').count() == 6
                && addr.split(':').all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit()));
            if is_address {
                phones.retain(|p| p != addr);
                phones.push(addr.to_string());
            }
        }
    }
    let skip = phones.len().saturating_sub(KEEP);
    phones.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_come_from_id_fields_newest_last() {
        let log = "\
            INFO attaching id=11:22:33:44:55:01 name=x\n\
            INFO phone subscribed id=in:AA:BB:CC:DD:EE:02\n\
            INFO link bound peer=0123456789abcdef id=nonsense\n\
            INFO attaching id=11:22:33:44:55:01 name=x\n";
        assert_eq!(recent_phones(log), ["AA:BB:CC:DD:EE:02", "11:22:33:44:55:01"]);
        assert!(recent_phones("no ids here").is_empty());
    }
}
